use std::collections::HashSet;

use anyhow::Context as _;
use futures::StreamExt;
use futures::pin_mut;
use presage::Manager;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::proto::{DataMessage, SyncMessage, sync_message::Sent};
use presage::libsignal_service::protocol::ServiceId;

pub struct ThreadEntry {
    pub thread: Thread,
    pub name: String,
    pub last_preview: Option<String>,
    pub last_ts: u64,
}

/// Drain the message queue until Signal reports it's empty.
/// Returns the set of threads seen during sync (including groups whose
/// metadata failed to fetch from the server).
pub async fn sync<S: Store>(
    manager: &mut Manager<S, Registered>,
) -> anyhow::Result<HashSet<Thread>> {
    eprintln!("Syncing...");
    let stream = manager
        .receive_messages()
        .await
        .context("failed to start receive stream")?;
    pin_mut!(stream);

    let mut count = 0usize;
    let mut seen: HashSet<Thread> = HashSet::new();

    while let Some(event) = stream.next().await {
        match event {
            Received::QueueEmpty => {
                eprintln!("Sync complete ({count} messages received).");
                break;
            }
            Received::Content(content) => {
                count += 1;
                if let Ok(thread) = Thread::try_from(content.as_ref()) {
                    seen.insert(thread);
                }
            }
            Received::Contacts => eprintln!("Contact list synced."),
        }
    }
    Ok(seen)
}

/// Build a chat list: contacts + groups + any threads seen during sync
/// that aren't covered by contacts/groups (e.g. Note to Self).
/// Sorted most-recent-first, empty threads omitted.
pub async fn list_threads<S: Store>(
    manager: &Manager<S, Registered>,
    own_aci: &ServiceId,
    extra_threads: HashSet<Thread>,
) -> anyhow::Result<Vec<ThreadEntry>> {
    let mut seen_threads: HashSet<Thread> = HashSet::new();
    let mut entries: Vec<ThreadEntry> = Vec::new();

    for result in manager.store().contacts().await? {
        let contact = result?;
        let service_id = ServiceId::Aci(contact.uuid.into());
        let thread = Thread::Contact(service_id);
        let name = if contact.name.is_empty() {
            contact
                .phone_number
                .map(|p| p.to_string())
                .unwrap_or_else(|| contact.uuid.to_string())
        } else {
            contact.name
        };
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        seen_threads.insert(thread.clone());
        if last_ts > 0 {
            entries.push(ThreadEntry { thread, name, last_preview, last_ts });
        }
    }

    for result in manager.store().groups().await? {
        let (master_key, group) = result?;
        let thread = Thread::Group(master_key);
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        seen_threads.insert(thread.clone());
        if last_ts > 0 {
            entries.push(ThreadEntry {
                thread,
                name: group.title,
                last_preview,
                last_ts,
            });
        }
    }

    // Handle threads seen during sync that weren't in contacts or groups
    // (typically: Note to Self group whose metadata failed to fetch from server).
    for thread in extra_threads {
        if seen_threads.contains(&thread) {
            continue;
        }
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts == 0 {
            continue;
        }
        let name = thread_name_fallback(manager, &thread, own_aci).await;
        seen_threads.insert(thread.clone());
        entries.push(ThreadEntry { thread, name, last_preview, last_ts });
    }

    entries.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    Ok(entries)
}

/// Name a thread we have no metadata for. Checks if all DataMessages in the
/// thread have our own ACI as sender — if so it's Note to Self.
async fn thread_name_fallback<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
    own_aci: &ServiceId,
) -> String {
    if let Thread::Group(_) = thread {
        let Ok(iter) = manager.store().messages(thread, ..).await else {
            return "Unknown Group".to_string();
        };
        let own_aci_str = own_aci.service_id_string();
        let all_self = iter
            .filter_map(|r| r.ok())
            .filter(|c| matches!(&c.body, ContentBody::DataMessage(_)))
            .all(|c| c.metadata.sender.service_id_string() == own_aci_str);
        if all_self {
            return "Note to Self".to_string();
        }
    }
    "Unknown Group".to_string()
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
    let ts = content.timestamp();
    let preview = extract_preview(&content);
    (preview, ts)
}

fn extract_preview(content: &Content) -> Option<String> {
    match &content.body {
        ContentBody::DataMessage(DataMessage { body: Some(text), .. }) => Some(text.clone()),
        ContentBody::SynchronizeMessage(SyncMessage {
            sent:
                Some(Sent {
                    message: Some(DataMessage { body: Some(text), .. }),
                    ..
                }),
            ..
        }) => Some(text.clone()),
        _ => None,
    }
}
