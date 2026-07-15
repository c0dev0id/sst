use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::pin::Pin;

use anyhow::Context as _;
use futures::{Stream, StreamExt};
use presage::Manager;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::prelude::Uuid;
use presage::libsignal_service::proto::{AttachmentPointer, DataMessage, EditMessage, GroupContextV2, ReceiptMessage, SyncMessage, data_message, receipt_message, sync_message::Sent};
use presage::libsignal_service::sender::AttachmentSpec;

/// Per-thread reaction state: target_ts → emoji → set of reactor UUID bytes.
/// Apply in chronological order to handle add/remove toggles correctly.
pub type ReactionMap = HashMap<u64, HashMap<String, HashSet<[u8; 16]>>>;

/// Format a per-emoji reaction sub-map into sorted `"NxE"` strings.
/// Shared by the status-bar hint in app.rs and the inline renderer in ui.rs.
pub(crate) fn fmt_reaction_pairs(map: &HashMap<String, HashSet<[u8; 16]>>) -> Vec<String> {
    let mut pairs: Vec<(&str, usize)> = map.iter()
        .map(|(e, s)| (e.as_str(), s.len()))
        .collect();
    pairs.sort_by_key(|&(e, _)| e);
    pairs.iter().map(|(e, c)| format!("{}x{}", c, e)).collect()
}

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

#[derive(Clone)]
pub struct StagedAttachment {
    pub path: PathBuf,
    pub kind: &'static str, // "gif" | "image" | "video" | "audio" | "file"
    pub mime: &'static str,
    pub size: u64,
}

pub fn stage_attachment(path: &Path) -> Result<StagedAttachment, String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    if !meta.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    let mime = mime_from_path(path);
    Ok(StagedAttachment {
        path: path.to_path_buf(),
        kind: kind_from_mime(mime),
        mime,
        size: meta.len(),
    })
}

fn mime_from_path(path: &Path) -> &'static str {
    let ext = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png"  => "image/png",
        "gif"  => "image/gif",
        "webp" => "image/webp",
        "heic" | "heif" => "image/heic",
        "mp4"  => "video/mp4",
        "mov"  => "video/quicktime",
        "avi"  => "video/x-msvideo",
        "webm" => "video/webm",
        "mkv"  => "video/x-matroska",
        "mp3"  => "audio/mpeg",
        "m4a"  => "audio/mp4",
        "ogg"  => "audio/ogg",
        "opus" => "audio/opus",
        "aac"  => "audio/aac",
        "flac" => "audio/flac",
        "wav"  => "audio/wav",
        "pdf"  => "application/pdf",
        "zip"  => "application/zip",
        "tar"  => "application/x-tar",
        "gz"   => "application/gzip",
        "bz2"  => "application/x-bzip2",
        _      => "application/octet-stream",
    }
}

fn kind_from_mime(mime: &str) -> &'static str {
    if mime == "image/gif" { "gif" }
    else if mime.starts_with("image/") { "image" }
    else if mime.starts_with("video/") { "video" }
    else if mime.starts_with("audio/") { "audio" }
    else { "file" }
}

pub fn fmt_attachment_size(bytes: u64) -> String {
    if bytes < 1_024 {
        format!("{} B", bytes)
    } else if bytes < 1_024 * 1_024 {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    } else if bytes < 1_024 * 1_024 * 1_024 {
        format!("{:.1} MB", bytes as f64 / (1_024.0 * 1_024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1_024.0 * 1_024.0 * 1_024.0))
    }
}

/// Upload staged attachments one by one; on any failure returns an error with
/// a human-readable message and the path of the file that failed, so the caller
/// can remove just that entry and let the user retry.
pub async fn upload_staged_attachments<S: Store>(
    manager: &mut Manager<S, Registered>,
    staged: &[StagedAttachment],
) -> Result<Vec<AttachmentPointer>, (String, PathBuf)> {
    let mut pointers = Vec::with_capacity(staged.len());
    for att in staged {
        let bytes = std::fs::read(&att.path)
            .map_err(|e| (format!("{}: {}", att.path.display(), e), att.path.clone()))?;
        let is_gif = att.mime == "image/gif";
        let spec = AttachmentSpec {
            content_type: att.mime.to_string(),
            length: bytes.len(),
            file_name: att.path.file_name().map(|n| n.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut pointer = match manager.upload_attachment(spec, bytes).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return Err((format!("upload failed: {e}"), att.path.clone())),
            Err(e)     => return Err((format!("upload error: {e}"), att.path.clone())),
        };
        if is_gif {
            // GIF = 8 per Signal proto's AttachmentPointer.Flags enum; not settable via AttachmentSpec.
            pointer.flags = Some(pointer.flags.unwrap_or(0) | 8);
        }
        pointers.push(pointer);
    }
    Ok(pointers)
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
                tracing::info!("sync complete ({count} messages)");
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
            Received::Contacts => tracing::info!("contact list synced"),
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

/// Drain the pending queue (discarding message content), request a fresh
/// contact list from the primary device, and wait for it to arrive.
/// Used for --contact-list mode; much faster than a full sync.
pub async fn sync_contacts<S: Store>(
    manager: &mut Manager<S, Registered>,
    state: &mut SyncState,
) -> anyhow::Result<()> {
    let own_aci = manager.whoami().await?.aci;
    state.own_aci = Some(own_aci);

    let mut stream = Box::pin(
        manager
            .receive_messages()
            .await
            .context("failed to start receive stream")?,
    );

    // Drain the queue without processing messages.
    // If Contacts arrives here (primary pushed it proactively), we're done.
    loop {
        match stream.next().await {
            Some(Received::QueueEmpty) => break,
            Some(Received::Contacts) => return Ok(()),
            Some(_) | None => {}
        }
    }

    // Ask the primary device to push an updated contact list.
    if let Err(e) = manager.request_contacts().await {
        tracing::warn!("request_contacts: {e}");
    }

    // Wait up to 10 s for the Contacts event.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(event) = stream.next().await {
            if matches!(event, Received::Contacts) {
                break;
            }
        }
    })
    .await;

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
    let preview = extract_preview(content);
    // Receipts, typing messages, and other no-body events have no preview.
    // Returning None here keeps the thread's last_preview and last_ts intact
    // so they can't clear a visible preview or push the thread to the top.
    if preview.is_none() {
        return None;
    }
    Some(MessageUpdate {
        thread,
        preview,
        ts: content.timestamp(),
    })
}

/// Build a chat list from contacts, known-but-unsynced contacts, and groups.
/// Sorted most-recent-first; threads with no messages are omitted.
pub async fn list_threads<S: Store>(
    manager: &Manager<S, Registered>,
    data_dir: &Path,
    own_aci: Option<Uuid>,
) -> anyhow::Result<Vec<ThreadEntry>> {
    let mut entries: Vec<ThreadEntry> = Vec::new();
    let mut seen: HashSet<Uuid> = HashSet::new();

    for result in manager.store().contacts().await? {
        let contact = result?;
        seen.insert(contact.uuid);
        let service_id = presage::libsignal_service::protocol::ServiceId::Aci(contact.uuid.into());
        let thread = Thread::Contact(service_id);
        let name = if own_aci == Some(contact.uuid) {
            "Note to Self".to_string()
        } else {
            contact_display_name(&contact)
        };
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts > 0 {
            entries.push(ThreadEntry { thread, name, last_preview, last_ts, unread: false });
        }
    }

    for uuid_bytes in load_contact_uuids(&data_dir.join("known_contacts")) {
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

/// Load every contact and group regardless of message history.
/// Returns `(entries, contacts_len)` where contacts come first (case-insensitive alpha)
/// followed by groups (case-insensitive alpha). Excludes the account owner.
pub async fn list_all_contacts<S: Store>(
    manager: &Manager<S, Registered>,
    own_aci: Option<Uuid>,
) -> anyhow::Result<(Vec<ThreadEntry>, usize)> {
    let mut contact_entries: Vec<ThreadEntry> = Vec::new();

    for result in manager.store().contacts().await? {
        let contact = result?;
        if own_aci == Some(contact.uuid) {
            continue;
        }
        let service_id = presage::libsignal_service::protocol::ServiceId::Aci(contact.uuid.into());
        let thread = Thread::Contact(service_id);
        let name = contact_display_name(&contact);
        contact_entries.push(ThreadEntry { thread, name, last_preview: None, last_ts: 0, unread: false });
    }
    contact_entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let contacts_len = contact_entries.len();

    let mut group_entries: Vec<ThreadEntry> = Vec::new();
    for result in manager.store().groups().await? {
        let (master_key, group) = result?;
        let thread = Thread::Group(master_key);
        group_entries.push(ThreadEntry { thread, name: group.title, last_preview: None, last_ts: 0, unread: false });
    }
    group_entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    contact_entries.extend(group_entries);
    Ok((contact_entries, contacts_len))
}

/// True for contacts that have no usable display name — no name field and no phone number.
/// These are candidates for a Signal profile fetch.
fn needs_profile_fetch(contact: &presage::model::contacts::Contact) -> bool {
    contact.name.is_empty() && contact.phone_number.is_none()
}

/// For contacts with no usable display name, try to fetch their Signal profile
/// using the profile key cached in the store. Returns a map of uuid → display name
/// for every contact that was successfully resolved.
pub async fn fetch_missing_profiles<S: Store>(
    manager: &mut Manager<S, Registered>,
) -> anyhow::Result<HashMap<Uuid, String>> {
    // Collect first: contacts() borrows &store immutably; the fetch loop below
    // needs &mut manager, which conflicts with holding the iterator.
    let nameless = {
        let mut v = Vec::new();
        for result in manager.store().contacts().await? {
            let c = result?;
            if needs_profile_fetch(&c) {
                v.push(c);
            }
        }
        v
    };

    let mut resolved = HashMap::new();
    for contact in nameless {
        let service_id = presage::libsignal_service::protocol::ServiceId::Aci(contact.uuid.into());
        let Ok(Some(key)) = manager.store().profile_key(&service_id).await else {
            continue;
        };
        match manager.retrieve_profile_by_uuid(contact.uuid, key).await {
            Ok(profile) => {
                if let Some(name) = &profile.name {
                    let s = name.to_string();
                    if !s.is_empty() {
                        resolved.insert(contact.uuid, s);
                    }
                }
            }
            Err(e) => tracing::warn!("profile fetch failed for {}: {e}", contact.uuid),
        }
    }
    Ok(resolved)
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

/// Look up a contact's display name by UUID; falls back to the UUID string.
pub async fn lookup_contact_name<S: Store>(
    manager: &Manager<S, Registered>,
    uuid: Uuid,
) -> String {
    let service_id = presage::libsignal_service::protocol::ServiceId::Aci(uuid.into());
    match manager.store().contact_by_id(&service_id).await {
        Ok(Some(contact)) => contact_display_name(&contact),
        _ => uuid.to_string(),
    }
}

/// Resolves display names for all participants in a thread (excluding self).
/// Returns a UUID → name map used for sender labels and @mention completion.
/// For group members not in the contact store, attempts a profile fetch using
/// the profile key stored in the group roster.
pub async fn load_sender_names<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    own_aci: Option<Uuid>,
) -> HashMap<Uuid, String> {
    let mut map: HashMap<Uuid, String> = HashMap::new();
    match thread {
        Thread::Group(key) => {
            let Ok(Some(group)) = manager.store().group(*key).await else {
                return map;
            };
            for member in group.members {
                let uuid = presage::libsignal_service::protocol::ServiceId::Aci(member.aci)
                    .raw_uuid();
                if own_aci == Some(uuid) {
                    continue;
                }
                let name = lookup_contact_name(manager, uuid).await;
                if name != uuid.to_string() {
                    map.insert(uuid, name);
                    continue;
                }
                // Not in contact store — try profile fetch using the roster key.
                match manager.retrieve_profile_by_uuid(uuid, member.profile_key).await {
                    Ok(profile) => {
                        let resolved = profile.name
                            .map(|n| n.to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| uuid.to_string());
                        map.insert(uuid, resolved);
                    }
                    Err(e) => {
                        tracing::warn!("profile fetch failed for {uuid}: {e}");
                        map.insert(uuid, uuid.to_string());
                    }
                }
            }
        }
        Thread::Contact(service_id) => {
            let uuid = service_id.raw_uuid();
            if own_aci != Some(uuid) {
                map.insert(uuid, lookup_contact_name(manager, uuid).await);
            }
        }
    }
    map
}

pub async fn load_messages<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
) -> anyhow::Result<Vec<Content>> {
    Ok(load_messages_and_reactions(manager, thread).await?.0)
}


/// Single-pass load: splits the thread's message store into displayable messages
/// and a reduced ReactionMap. Avoids two separate full scans when both are needed.
///
/// Extract (target_sent_timestamp, new_body) from an EditMessage or a SyncMessage
/// wrapping an edit (sent by us on another device). Returns None for all other content.
fn extract_edit(content: &Content) -> Option<(u64, String)> {
    match &content.body {
        ContentBody::EditMessage(EditMessage {
            target_sent_timestamp: Some(ts),
            data_message: Some(dm),
        }) => Some((*ts, data_message_body(dm))),
        ContentBody::SynchronizeMessage(SyncMessage {
            sent: Some(Sent {
                edit_message: Some(EditMessage {
                    target_sent_timestamp: Some(ts),
                    data_message: Some(dm),
                }),
                ..
            }),
            ..
        }) => Some((*ts, data_message_body(dm))),
        _ => None,
    }
}

/// Replace the body text of a DataMessage or SyncMessage in-place.
fn apply_edit_body(content: &mut Content, new_body: &str) {
    match &mut content.body {
        ContentBody::DataMessage(dm) => {
            dm.body = Some(new_body.to_string());
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            sent: Some(Sent { message: Some(dm), .. }),
            ..
        }) => {
            dm.body = Some(new_body.to_string());
        }
        _ => {}
    }
}

/// Reaction toggle semantics: events are processed in chronological order (ASC).
/// The same (sender, emoji, target_ts) triple with remove=true undoes a prior add.
pub async fn load_messages_and_reactions<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
) -> anyhow::Result<(Vec<Content>, ReactionMap)> {
    let iter = manager
        .store()
        .messages(thread, ..)
        .await
        .context("failed to load messages")?;

    let mut messages: Vec<Content> = Vec::new();
    let mut events: Vec<(u64, [u8; 16], String, bool, u64)> = Vec::new();
    // target_sent_timestamp → latest edited body. Store returns DESC, so the first
    // EditMessage seen for a given target is the most recent edit.
    let mut edits: HashMap<u64, String> = HashMap::new();

    for result in iter {
        let Ok(content) = result else { continue };

        // Collect edit messages; they replace the original rather than appearing standalone.
        if let Some((target_ts, new_body)) = extract_edit(&content) {
            edits.entry(target_ts).or_insert(new_body);
            continue;
        }

        let reaction_opt = match &content.body {
            ContentBody::DataMessage(msg) => {
                msg.reaction.as_ref().map(|r| (r, content.metadata.sender.raw_uuid()))
            }
            ContentBody::SynchronizeMessage(SyncMessage {
                sent: Some(Sent { message: Some(msg), .. }),
                ..
            }) => {
                msg.reaction.as_ref().map(|r| (r, content.metadata.sender.raw_uuid()))
            }
            _ => None,
        };

        if let Some((reaction, sender_uuid)) = reaction_opt {
            if let (Some(emoji), Some(target_ts)) = (
                reaction.emoji.as_ref().filter(|e| !e.is_empty()),
                reaction.target_sent_timestamp,
            ) {
                events.push((
                    content.timestamp(),
                    *sender_uuid.as_bytes(),
                    emoji.clone(),
                    reaction.remove.unwrap_or(false),
                    target_ts,
                ));
            }
        } else if !message_body(&content).is_empty() {
            messages.push(content);
        }
    }

    // Store returns DESC; both slices need ASC order.
    messages.reverse();
    events.reverse();

    // Apply edits in-place: replace the body of the original message.
    for msg in &mut messages {
        if let Some(new_body) = edits.get(&msg.timestamp()) {
            apply_edit_body(msg, new_body);
        }
    }

    let mut reactions: ReactionMap = HashMap::new();
    for (_, sender_bytes, emoji, remove, target_ts) in events {
        let per_emoji = reactions.entry(target_ts).or_default().entry(emoji).or_default();
        if remove {
            per_emoji.remove(&sender_bytes);
        } else {
            per_emoji.insert(sender_bytes);
        }
    }
    for emoji_map in reactions.values_mut() {
        emoji_map.retain(|_, senders| !senders.is_empty());
    }
    reactions.retain(|_, emoji_map| !emoji_map.is_empty());

    Ok((messages, reactions))
}

/// Delete a message for everyone: sends a DataMessage.delete to the thread
/// (Signal fans it out to all recipients/group members), then removes it from
/// the local store. Only the original sender can delete their own messages.
pub async fn delete_for_everyone<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    timestamp: u64,
) -> anyhow::Result<()> {
    let ts = now_millis()?;
    let data_message = DataMessage {
        delete: Some(data_message::Delete {
            target_sent_timestamp: Some(timestamp),
        }),
        timestamp: Some(ts),
        ..Default::default()
    };
    dispatch_send(manager, thread, data_message, ts).await?;
    let mut store = manager.store().clone();
    store
        .delete_message(thread, timestamp)
        .await
        .map_err(|e| anyhow::anyhow!("local delete_message: {e}"))?;
    Ok(())
}

pub async fn send_reaction<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    emoji: String,
    target_ts: u64,
    target_author: Uuid,
    remove: bool,
) -> anyhow::Result<()> {
    let ts = now_millis()?;
    let reaction = data_message::Reaction {
        emoji: Some(emoji),
        remove: Some(remove),
        target_author_aci: Some(target_author.to_string()),
        target_author_aci_binary: Some(target_author.as_bytes().to_vec()),
        target_sent_timestamp: Some(target_ts),
        ..Default::default()
    };
    let data_message = DataMessage {
        reaction: Some(reaction),
        timestamp: Some(ts),
        ..Default::default()
    };
    dispatch_send(manager, thread, data_message, ts).await
}

pub async fn send_edit<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    target_ts: u64,
    body: String,
) -> anyhow::Result<()> {
    let ts = now_millis()?;
    let data_message = DataMessage {
        body: Some(body),
        timestamp: Some(ts),
        ..Default::default()
    };
    let edit = ContentBody::EditMessage(EditMessage {
        target_sent_timestamp: Some(target_ts),
        data_message: Some(data_message),
    });
    dispatch_send_body(manager, thread, edit, ts).await
}

pub async fn send_to_thread<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    text: String,
    attachments: Vec<AttachmentPointer>,
) -> anyhow::Result<()> {
    let ts = now_millis()?;
    let data_message = DataMessage {
        body: if text.is_empty() { None } else { Some(text) },
        timestamp: Some(ts),
        attachments,
        ..Default::default()
    };
    dispatch_send(manager, thread, data_message, ts).await
}

pub async fn send_reply<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    text: String,
    quote_ts: u64,
    quote_author: Uuid,
    quote_text: String,
    attachments: Vec<AttachmentPointer>,
) -> anyhow::Result<()> {
    let ts = now_millis()?;
    let quote = data_message::Quote {
        id: Some(quote_ts),
        author_aci: Some(quote_author.to_string()),
        author_aci_binary: Some(quote_author.as_bytes().to_vec()),
        text: if quote_text.is_empty() { None } else { Some(quote_text) },
        ..Default::default()
    };
    let data_message = DataMessage {
        body: if text.is_empty() { None } else { Some(text) },
        timestamp: Some(ts),
        quote: Some(quote),
        attachments,
        ..Default::default()
    };
    dispatch_send(manager, thread, data_message, ts).await
}

/// Returns the (author_uuid, first_line_of_text) for the quoted message, if any.
pub fn message_quote(content: &Content) -> Option<(Uuid, String)> {
    let quote = match &content.body {
        ContentBody::DataMessage(msg) => msg.quote.as_ref()?,
        _ => return None,
    };
    // Prefer the binary ACI field; fall back to the string form.
    let author = if let Some(bytes) = &quote.author_aci_binary {
        Uuid::from_slice(bytes).ok()?
    } else {
        Uuid::parse_str(quote.author_aci.as_deref()?).ok()?
    };
    let text = quote.text.clone().unwrap_or_default();
    Some((author, text))
}

/// Returns (delivered_timestamps, read_timestamps) for our sent messages in this thread.
/// Scans stored ReceiptMessage entries; read implies delivered.
pub async fn load_receipt_state<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
) -> anyhow::Result<(HashSet<u64>, HashSet<u64>)> {
    let iter = manager
        .store()
        .messages(thread, ..)
        .await
        .context("failed to load messages for receipt state")?;

    let mut delivered: HashSet<u64> = HashSet::new();
    let mut read: HashSet<u64> = HashSet::new();

    for result in iter {
        let Ok(content) = result else { continue };
        if let ContentBody::ReceiptMessage(receipt) = &content.body {
            let kind = receipt.r#type.and_then(|t| receipt_message::Type::try_from(t).ok());
            match kind {
                Some(receipt_message::Type::Delivery) => {
                    delivered.extend(receipt.timestamp.iter().copied());
                }
                Some(receipt_message::Type::Read) => {
                    // Read implies delivered
                    read.extend(receipt.timestamp.iter().copied());
                    delivered.extend(receipt.timestamp.iter().copied());
                }
                _ => {}
            }
        }
    }

    Ok((delivered, read))
}

/// Send a READ receipt to the contact for the given message timestamps.
/// Silently skips group threads (would need per-sender dispatch).
pub async fn send_read_receipt<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    timestamps: Vec<u64>,
) -> anyhow::Result<()> {
    if timestamps.is_empty() {
        return Ok(());
    }
    let service_id = match thread {
        Thread::Contact(sid) => sid.clone(),
        Thread::Group(_) => return Ok(()),
    };
    let ts = now_millis()?;
    let receipt = ReceiptMessage {
        r#type: Some(receipt_message::Type::Read as i32),
        timestamp: timestamps,
    };
    manager
        .send_message(service_id, receipt, ts)
        .await
        .context("failed to send read receipt")?;
    Ok(())
}

fn now_millis() -> anyhow::Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system time error")?
        .as_millis() as u64)
}

async fn dispatch_send<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    data_message: DataMessage,
    ts: u64,
) -> anyhow::Result<()> {
    dispatch_send_body(manager, thread, ContentBody::DataMessage(data_message), ts).await
}

async fn dispatch_send_body<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    mut body: ContentBody,
    ts: u64,
) -> anyhow::Result<()> {
    // Signal requires GroupContextV2 in every DataMessage sent to a group.
    // Without it, recipients treat each delivery as an individual DM from the sender.
    // presage's send_message_to_group does NOT inject this — it's our responsibility.
    if let Thread::Group(master_key) = thread {
        let revision = manager.store().group(*master_key).await
            .ok()
            .flatten()
            .map(|g| g.revision)
            .unwrap_or(0);
        let ctx = GroupContextV2 {
            master_key: Some(master_key.to_vec()),
            revision: Some(revision),
            group_change: None,
        };
        match &mut body {
            ContentBody::DataMessage(dm) => {
                dm.group_v2 = Some(ctx);
            }
            ContentBody::EditMessage(edit) => {
                if let Some(dm) = &mut edit.data_message {
                    dm.group_v2 = Some(ctx);
                }
            }
            _ => {}
        }
    }
    match thread {
        Thread::Contact(service_id) => {
            manager
                .send_message(service_id.clone(), body, ts)
                .await
                .context("failed to send message")?;
        }
        Thread::Group(master_key) => {
            manager
                .send_message_to_group(master_key, body, ts)
                .await
                .context("failed to send message to group")?;
        }
    }
    Ok(())
}
