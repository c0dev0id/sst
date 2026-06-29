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
/// presage saves every received message to the SQLite store automatically.
pub async fn sync<S: Store>(manager: &mut Manager<S, Registered>) -> anyhow::Result<()> {
    eprintln!("Syncing...");
    let stream = manager
        .receive_messages()
        .await
        .context("failed to start receive stream")?;
    pin_mut!(stream);

    let mut count = 0usize;
    while let Some(event) = stream.next().await {
        match event {
            Received::QueueEmpty => {
                eprintln!("Sync complete ({count} messages received).");
                break;
            }
            Received::Content(_) => count += 1,
            Received::Contacts => eprintln!("Contact list synced."),
        }
    }
    Ok(())
}

/// Build a chat list: all contacts + groups, sorted most-recent-first.
pub async fn list_threads<S: Store>(
    manager: &Manager<S, Registered>,
) -> anyhow::Result<Vec<ThreadEntry>> {
    let mut entries: Vec<ThreadEntry> = Vec::new();

    for result in manager.store().contacts().await? {
        let contact = result?;
        let service_id = ServiceId::Aci(contact.uuid.into());
        let thread = Thread::Contact(service_id);
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        let name = if contact.name.is_empty() {
            contact
                .phone_number
                .map(|p| p.to_string())
                .unwrap_or_else(|| contact.uuid.to_string())
        } else {
            contact.name
        };
        entries.push(ThreadEntry { thread, name, last_preview, last_ts });
    }

    for result in manager.store().groups().await? {
        let (master_key, group) = result?;
        let thread = Thread::Group(master_key);
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        entries.push(ThreadEntry {
            thread,
            name: group.title,
            last_preview,
            last_ts,
        });
    }

    entries.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    Ok(entries)
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
