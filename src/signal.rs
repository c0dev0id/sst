use std::collections::HashSet;
use std::path::Path;

use anyhow::Context as _;
use futures::StreamExt;
use futures::pin_mut;
use presage::Manager;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::prelude::Uuid;
use presage::libsignal_service::proto::{DataMessage, GroupContextV2, SyncMessage, sync_message::Sent};

pub struct ThreadEntry {
    pub thread: Thread,
    pub name: String,
    pub last_preview: Option<String>,
    pub last_ts: u64,
}

pub struct SyncState {
    pub data_dir: std::path::PathBuf,
}

impl SyncState {
    fn groups_path(&self) -> std::path::PathBuf {
        self.data_dir.join("known_groups")
    }

    fn contacts_path(&self) -> std::path::PathBuf {
        self.data_dir.join("known_contacts")
    }
}

/// Drain the message queue, persist discovered threads, and refresh group metadata.
pub async fn sync<S: Store>(
    manager: &mut Manager<S, Registered>,
    state: &SyncState,
) -> anyhow::Result<()> {
    eprintln!("Syncing...");

    let own_aci = manager.whoami().await?.aci;

    let stream = manager
        .receive_messages()
        .await
        .context("failed to start receive stream")?;
    pin_mut!(stream);

    let mut count = 0usize;
    let mut seen_group_keys: HashSet<[u8; 32]> = HashSet::new();
    let mut seen_contact_uuids: HashSet<[u8; 16]> = HashSet::new();

    while let Some(event) = stream.next().await {
        match event {
            Received::QueueEmpty => {
                eprintln!("Sync complete ({count} messages received).");
                break;
            }
            Received::Content(content) => {
                count += 1;
                match Thread::try_from(content.as_ref()) {
                    Ok(Thread::Group(key)) => {
                        seen_group_keys.insert(key);
                    }
                    Ok(Thread::Contact(service_id)) => {
                        let uuid = service_id.raw_uuid();
                        if uuid != own_aci {
                            seen_contact_uuids.insert(*uuid.as_bytes());
                        }
                    }
                    Err(_) => {}
                }
            }
            Received::Contacts => eprintln!("Contact list synced."),
        }
    }

    // Persist and refresh group metadata.
    let mut known_groups = load_group_keys(&state.groups_path());
    known_groups.extend(seen_group_keys);
    let _ = save_group_keys(&state.groups_path(), &known_groups);

    for key in &known_groups {
        if manager.store().group(*key).await?.is_some() {
            continue;
        }
        let ctx = GroupContextV2 {
            master_key: Some(key.to_vec()),
            revision: Some(0),
            group_change: None,
        };
        if let Err(e) = manager.retrieve_group_avatar(ctx).await {
            eprintln!("Warning: could not fetch group metadata: {e}");
        }
    }

    // Persist newly discovered contact UUIDs.
    let mut known_contacts = load_contact_uuids(&state.contacts_path());
    known_contacts.extend(seen_contact_uuids);
    let _ = save_contact_uuids(&state.contacts_path(), &known_contacts);

    // Ask the primary device to push its contact list.
    // The response arrives as Received::Contacts on the next sync.
    if let Err(e) = manager.request_contacts().await {
        eprintln!("Warning: could not request contact sync: {e}");
    }

    Ok(())
}

/// Build a chat list from contacts, known-but-unsynced contacts, and groups.
/// Sorted most-recent-first; threads with no messages are omitted.
pub async fn list_threads<S: Store>(
    manager: &Manager<S, Registered>,
    state: &SyncState,
) -> anyhow::Result<Vec<ThreadEntry>> {
    let mut entries: Vec<ThreadEntry> = Vec::new();
    let mut seen: HashSet<Uuid> = HashSet::new();

    // Contacts from the contacts table (have names).
    for result in manager.store().contacts().await? {
        let contact = result?;
        seen.insert(contact.uuid);
        let service_id = presage::libsignal_service::protocol::ServiceId::Aci(contact.uuid.into());
        let thread = Thread::Contact(service_id);
        let name = contact_display_name(&contact);
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts > 0 {
            entries.push(ThreadEntry { thread, name, last_preview, last_ts });
        }
    }

    // Contacts discovered via message sync but absent from the contacts table.
    for uuid_bytes in load_contact_uuids(&state.contacts_path()) {
        let uuid = Uuid::from_bytes(uuid_bytes);
        if seen.contains(&uuid) {
            continue;
        }
        seen.insert(uuid);
        let service_id = presage::libsignal_service::protocol::ServiceId::Aci(uuid.into());
        let thread = Thread::Contact(service_id.clone());
        let name = match manager.store().contact_by_id(&service_id).await {
            Ok(Some(contact)) => contact_display_name(&contact),
            _ => uuid.to_string(),
        };
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts > 0 {
            entries.push(ThreadEntry { thread, name, last_preview, last_ts });
        }
    }

    // Groups with fetched metadata.
    for result in manager.store().groups().await? {
        let (master_key, group) = result?;
        let thread = Thread::Group(master_key);
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts > 0 {
            entries.push(ThreadEntry { thread, name: group.title, last_preview, last_ts });
        }
    }

    entries.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    Ok(entries)
}

fn contact_display_name(contact: &presage::model::contacts::Contact) -> String {
    if !contact.name.is_empty() {
        return contact.name.clone();
    }
    contact
        .phone_number
        .as_ref()
        .map(|p| p.to_string())
        .unwrap_or_else(|| contact.uuid.to_string())
}

fn load_group_keys(path: &Path) -> HashSet<[u8; 32]> {
    std::fs::read(path)
        .unwrap_or_default()
        .chunks_exact(32)
        .filter_map(|c| c.try_into().ok())
        .collect()
}

fn save_group_keys(path: &Path, keys: &HashSet<[u8; 32]>) -> std::io::Result<()> {
    let bytes: Vec<u8> = keys.iter().flat_map(|k| k.iter().copied()).collect();
    std::fs::write(path, bytes)
}

fn load_contact_uuids(path: &Path) -> HashSet<[u8; 16]> {
    std::fs::read(path)
        .unwrap_or_default()
        .chunks_exact(16)
        .filter_map(|c| c.try_into().ok())
        .collect()
}

fn save_contact_uuids(path: &Path, uuids: &HashSet<[u8; 16]>) -> std::io::Result<()> {
    let bytes: Vec<u8> = uuids.iter().flat_map(|k| k.iter().copied()).collect();
    std::fs::write(path, bytes)
}

async fn last_message<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
) -> (Option<String>, u64) {
    let Ok(mut iter) = manager.store().messages(thread, ..).await else {
        return (None, 0);
    };
    let Some(Ok(content)) = iter.next() else {
        return (None, 0);
    };
    (extract_preview(&content), content.timestamp())
}

fn extract_preview(content: &Content) -> Option<String> {
    match &content.body {
        ContentBody::DataMessage(DataMessage { body: Some(text), .. }) => Some(text.clone()),
        ContentBody::SynchronizeMessage(SyncMessage {
            sent: Some(Sent { message: Some(DataMessage { body: Some(text), .. }), .. }),
            ..
        }) => Some(text.clone()),
        _ => None,
    }
}
