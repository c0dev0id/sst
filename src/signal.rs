use std::collections::HashSet;
use std::path::Path;
use std::pin::Pin;

use anyhow::Context as _;
use futures::{Stream, StreamExt};
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
    pub unread: bool,
}

pub struct MessageUpdate {
    pub thread: Thread,
    pub preview: Option<String>,
    pub ts: u64,
}

pub struct SyncState {
    pub data_dir: std::path::PathBuf,
    pub own_aci: Option<Uuid>,
}

impl SyncState {
    fn groups_path(&self) -> std::path::PathBuf {
        self.data_dir.join("known_groups")
    }

    fn contacts_path(&self) -> std::path::PathBuf {
        self.data_dir.join("known_contacts")
    }
}

async fn drain_backlog<S: Store>(
    stream: &mut (impl Stream<Item = Received> + Unpin),
    manager: &mut Manager<S, Registered>,
    state: &mut SyncState,
) -> anyhow::Result<()> {
    let own_aci = manager.whoami().await?.aci;
    state.own_aci = Some(own_aci);

    let mut count = 0usize;
    let mut seen_group_keys: HashSet<[u8; 32]> = HashSet::new();
    let mut seen_contact_uuids: HashSet<[u8; 16]> = HashSet::new();

    while let Some(event) = stream.next().await {
        match event {
            Received::QueueEmpty => {
                eprintln!("Sync complete ({count} messages).");
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

    let mut known_contacts = load_contact_uuids(&state.contacts_path());
    known_contacts.extend(seen_contact_uuids);
    let _ = save_contact_uuids(&state.contacts_path(), &known_contacts);

    if let Err(e) = manager.request_contacts().await {
        eprintln!("Warning: could not request contact sync: {e}");
    }

    Ok(())
}

/// Drain the message queue and drop the stream. Used for --list mode.
pub async fn sync<S: Store>(
    manager: &mut Manager<S, Registered>,
    state: &mut SyncState,
) -> anyhow::Result<()> {
    let mut stream = Box::pin(
        manager
            .receive_messages()
            .await
            .context("failed to start receive stream")?,
    );
    drain_backlog(&mut stream, manager, state).await
}

/// Drain the message queue then return the live stream for TUI mode.
pub async fn connect<S: Store>(
    manager: &mut Manager<S, Registered>,
    state: &mut SyncState,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = Received>>>> {
    let mut stream: Pin<Box<dyn Stream<Item = Received>>> = Box::pin(
        manager
            .receive_messages()
            .await
            .context("failed to start receive stream")?,
    );
    drain_backlog(&mut stream, manager, state).await?;
    Ok(stream)
}

pub fn extract_update(content: &Content) -> Option<MessageUpdate> {
    let thread = Thread::try_from(content).ok()?;
    Some(MessageUpdate {
        thread,
        preview: extract_preview(content),
        ts: content.timestamp(),
    })
}

/// Build a chat list from contacts, known-but-unsynced contacts, and groups.
/// Sorted most-recent-first; threads with no messages are omitted.
pub async fn list_threads<S: Store>(
    manager: &Manager<S, Registered>,
    state: &SyncState,
) -> anyhow::Result<Vec<ThreadEntry>> {
    let mut entries: Vec<ThreadEntry> = Vec::new();
    let mut seen: HashSet<Uuid> = HashSet::new();

    for result in manager.store().contacts().await? {
        let contact = result?;
        seen.insert(contact.uuid);
        let service_id = presage::libsignal_service::protocol::ServiceId::Aci(contact.uuid.into());
        let thread = Thread::Contact(service_id);
        let name = if state.own_aci == Some(contact.uuid) {
            "Note to Self".to_string()
        } else {
            contact_display_name(&contact)
        };
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts > 0 {
            entries.push(ThreadEntry { thread, name, last_preview, last_ts, unread: false });
        }
    }

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
            entries.push(ThreadEntry { thread, name, last_preview, last_ts, unread: false });
        }
    }

    for result in manager.store().groups().await? {
        let (master_key, group) = result?;
        let thread = Thread::Group(master_key);
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts > 0 {
            entries.push(ThreadEntry { thread, name: group.title, last_preview, last_ts, unread: false });
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
    let Ok(iter) = manager.store().messages(thread, ..).await else {
        return (None, 0);
    };
    let mut last_ts = 0u64;
    let mut last_preview: Option<String> = None;
    for result in iter.take(50) {
        let Ok(content) = result else { continue };
        let ts = content.timestamp();
        if last_ts == 0 {
            last_ts = ts; // most recent message timestamp, for sort order
        }
        if last_preview.is_none() {
            last_preview = extract_preview(&content);
            if last_preview.is_some() {
                break;
            }
        }
    }
    (last_preview, last_ts)
}

fn extract_preview(content: &Content) -> Option<String> {
    let body = message_body(content);
    if body.is_empty() { None } else { Some(body) }
}

pub fn message_body(content: &Content) -> String {
    match &content.body {
        ContentBody::DataMessage(msg) => data_message_body(msg),
        ContentBody::SynchronizeMessage(SyncMessage {
            sent: Some(Sent { message: Some(msg), .. }),
            ..
        }) => data_message_body(msg),
        ContentBody::CallMessage(_) => "Call".to_string(),
        _ => String::new(),
    }
}

fn data_message_body(msg: &DataMessage) -> String {
    if let Some(text) = &msg.body {
        if !text.is_empty() {
            return text.clone();
        }
    }
    if !msg.attachments.is_empty() {
        return "Attachment".to_string();
    }
    if msg.sticker.is_some() {
        return "Sticker".to_string();
    }
    String::new()
}

pub async fn load_messages<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
) -> anyhow::Result<Vec<Content>> {
    let iter = manager
        .store()
        .messages(thread, ..)
        .await
        .context("failed to load messages")?;
    let mut messages: Vec<Content> = iter
        .filter_map(|r| r.ok())
        .filter(|c| !message_body(c).is_empty())
        .collect();
    messages.reverse(); // store returns DESC (newest first), display needs ASC
    Ok(messages)
}

pub async fn send_to_thread<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    text: String,
) -> anyhow::Result<()> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system time error")?
        .as_millis() as u64;
    let data_message = DataMessage {
        body: Some(text),
        timestamp: Some(ts),
        ..Default::default()
    };
    match thread {
        Thread::Contact(service_id) => {
            manager
                .send_message(service_id.clone(), data_message, ts)
                .await
                .context("failed to send message")?;
        }
        Thread::Group(master_key) => {
            manager
                .send_message_to_group(master_key, data_message, ts)
                .await
                .context("failed to send message to group")?;
        }
    }
    Ok(())
}
