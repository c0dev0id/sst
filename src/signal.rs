use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use anyhow::Context as _;
use chrono::{DateTime, Local, Utc};
use futures::{Stream, StreamExt};
use presage::Manager;
use presage::manager::Registered;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::libsignal_service::content::{Content, ContentBody};
use presage::libsignal_service::prelude::Uuid;
use presage::libsignal_service::protocol::ServiceId;
use presage::libsignal_service::proto::{AttachmentPointer, BodyRange, DataMessage, EditMessage, GroupContextV2, ReceiptMessage, SyncMessage, data_message, data_message::quote::QuotedAttachment, receipt_message, sync_message::{self, Sent}};

use crate::attachments::{stage_attachment, upload_staged_attachments};
use crate::mentions::{encode_mentions, mention_ranges, mentioned_acis, mentions_account, substitute_mentions};

/// Per-thread reaction state: target_ts → emoji → set of reactor UUID bytes.
/// Apply in chronological order to handle add/remove toggles correctly.
pub type ReactionMap = HashMap<u64, HashMap<String, HashSet<[u8; 16]>>>;

/// If an error looks like Signal rejected this device's credentials, return
/// a user-facing message. Walks the anyhow chain and matches on Display text
/// because presage bubbles WS-upgrade 403s through `ServiceError::WsError`
/// without a dedicated variant, and downcasting would drag libsignal_service
/// types into main.rs.
pub fn relink_hint(err: &anyhow::Error) -> Option<&'static str> {
    for cause in err.chain() {
        let s = cause.to_string();
        if s.contains("403")
            || s.contains("Forbidden")
            || s.contains("Authorization failed")
            || s.contains("please relink")
            || s.contains("Unauthorized")
        {
            return Some(
                "Signal rejected this device. \
                 Run `sst link` to re-provision.",
            );
        }
    }
    None
}

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

impl ThreadEntry {
    // Constructors never set unread=true: the data source doesn't know
    // whether the user has seen the message. Only the live-event fold in
    // app.rs marks a thread unread.
    pub fn new(thread: Thread, name: String, last_preview: Option<String>, last_ts: u64) -> Self {
        Self { thread, name, last_preview, last_ts, unread: false }
    }
}

pub struct MessageUpdate {
    pub thread: Thread,
    pub sender: Uuid,
    pub preview: String,
    pub ts: u64,              // when this message, or this edit, was sent
    pub edit_of: Option<u64>, // for an edit, the timestamp of the message it replaces
}

/// A thread from its CLI form: a contact UUID or a 64-char hex group master key.
pub fn parse_thread_id(s: &str) -> anyhow::Result<Thread> {
    if let Ok(uuid) = s.parse::<Uuid>() {
        return Ok(Thread::Contact(ServiceId::Aci(uuid.into())));
    }
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16))
            .collect::<Result<_, _>>()?;
        let key: [u8; 32] = bytes.try_into().expect("32 bytes");
        return Ok(Thread::Group(key));
    }
    anyhow::bail!("invalid recipient '{s}': expected a UUID (contact) or 64-char hex string (group)")
}

pub(crate) fn sort_recent_first(entries: &mut [ThreadEntry]) {
    entries.sort_by_key(|e| std::cmp::Reverse(e.last_ts));
}

pub fn is_own(content: &Content, own_aci: Option<Uuid>) -> bool {
    own_aci == Some(content.metadata.sender.raw_uuid())
}


// Side files next to the store: groups and contacts seen in message traffic,
// which presage does not enumerate on its own.
fn known_groups_path(data_dir: &Path) -> PathBuf {
    data_dir.join("known_groups")
}

fn known_contacts_path(data_dir: &Path) -> PathBuf {
    data_dir.join("known_contacts")
}

async fn drain_backlog<S: Store>(
    stream: &mut (impl Stream<Item = Received> + Unpin),
    manager: &mut Manager<S, Registered>,
    data_dir: &Path,
    show_progress: bool,
) -> anyhow::Result<()> {
    let own = own_aci(manager);

    let mut count = 0usize;
    let mut seen_group_keys: HashSet<[u8; 32]> = HashSet::new();
    let mut seen_contact_uuids: HashSet<[u8; 16]> = HashSet::new();

    while let Some(event) = stream.next().await {
        match event {
            Received::QueueEmpty => {
                if show_progress {
                    if count > 0 {
                        // Trailing spaces overwrite leftover digits from the longer counter line.
                        eprintln!("\rSynced {count} messages.        ");
                    } else {
                        eprintln!("Already up to date.");
                    }
                }
                tracing::info!("sync complete ({count} messages)");
                break;
            }
            Received::Content(content) => {
                count += 1;
                if show_progress {
                    eprint!("\rSyncing messages… {count}");
                    let _ = std::io::stderr().flush();
                }
                match Thread::try_from(content.as_ref()) {
                    Ok(Thread::Group(key)) => {
                        seen_group_keys.insert(key);
                    }
                    Ok(Thread::Contact(service_id)) => {
                        let uuid = service_id.raw_uuid();
                        if uuid != own {
                            seen_contact_uuids.insert(*uuid.as_bytes());
                        }
                    }
                    Err(_) => {}
                }
            }
            Received::Contacts => tracing::info!("contact list synced"),
        }
    }

    let mut known_groups = load_chunks::<32>(&known_groups_path(data_dir));
    known_groups.extend(seen_group_keys);
    let _ = save_chunks(&known_groups_path(data_dir), &known_groups);

    let mut announced_group_fetch = false;
    for key in &known_groups {
        if manager.store().group(*key).await?.is_some() {
            continue;
        }
        if show_progress && !announced_group_fetch {
            eprintln!("Fetching group metadata…");
            announced_group_fetch = true;
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

    let mut known_contacts = load_chunks::<16>(&known_contacts_path(data_dir));
    known_contacts.extend(seen_contact_uuids);
    let _ = save_chunks(&known_contacts_path(data_dir), &known_contacts);

    if let Err(e) = manager.request_contacts().await {
        eprintln!("Warning: could not request contact sync: {e}");
    }

    Ok(())
}

/// Drain the pending queue (discarding message content), request a fresh
/// contact list from the primary device, and wait for it to arrive.
/// Much faster than a full sync when only the contact list is needed.
pub async fn sync_contacts<S: Store>(manager: &mut Manager<S, Registered>) -> anyhow::Result<()> {
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
            Some(Received::Content(_)) => {}
            None => anyhow::bail!("receive stream closed before the message queue was drained"),
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

/// Drain the pending message queue to force a resync from the primary device.
pub async fn sync<S: Store>(manager: &mut Manager<S, Registered>, data_dir: &Path) -> anyhow::Result<()> {
    let mut stream = Box::pin(
        manager
            .receive_messages()
            .await
            .context("failed to start receive stream")?,
    );
    drain_backlog(&mut stream, manager, data_dir, false).await
}

/// Drain the message queue then return the live stream for TUI mode.
pub async fn connect<S: Store>(
    manager: &mut Manager<S, Registered>,
    data_dir: &Path,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = Received>>>> {
    eprintln!("Connecting to Signal…");
    let mut stream: Pin<Box<dyn Stream<Item = Received>>> = Box::pin(
        manager
            .receive_messages()
            .await
            .context("failed to start receive stream")?,
    );
    drain_backlog(&mut stream, manager, data_dir, true).await?;
    Ok(stream)
}

/// A receipt arriving live. Its timestamps name our messages wherever they were
/// sent, so it can be folded into the open chat without touching the store.
pub fn live_receipt(received: &Received) -> Option<&ReceiptMessage> {
    match received {
        Received::Content(content) => match &content.body {
            ContentBody::ReceiptMessage(receipt) => Some(receipt),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a live event is a message in `thread` (text, reaction, edit or
/// delete), as opposed to a receipt, typing indicator or queue marker.
pub fn is_message_in(received: &Received, thread: &Thread) -> bool {
    let Received::Content(content) = received else { return false };
    !matches!(content.body, ContentBody::TypingMessage(_) | ContentBody::ReceiptMessage(_))
        && Thread::try_from(content.as_ref()).ok().as_ref() == Some(thread)
}

/// The chat-list effect of a live event: the thread's new preview and the
/// timestamp of the message it describes. Receipts, typing and other bodiless
/// events yield None so they cannot clear a preview or reorder the list.
pub fn extract_update(content: &Content, name_of: impl Fn(Uuid) -> String) -> Option<MessageUpdate> {
    let thread = Thread::try_from(content).ok()?;
    let (preview, ts, edit_of) = match extract_edit(content) {
        Some((target_ts, edited)) => {
            let sent_ts = edited.timestamp.unwrap_or(target_ts);
            (Shown::of_message(&edited).render(name_of), sent_ts, Some(target_ts))
        }
        None => (message_body(content, name_of), content.timestamp(), None),
    };
    if preview.is_empty() {
        return None;
    }
    let sender = content.metadata.sender.raw_uuid();
    Some(MessageUpdate { thread, sender, preview, ts, edit_of })
}

/// Record a contact first seen in live traffic, so later runs enumerate their thread.
pub fn remember_contact(data_dir: &Path, uuid: Uuid) {
    let path = known_contacts_path(data_dir);
    let mut known = load_chunks::<16>(&path);
    if known.insert(*uuid.as_bytes()) {
        let _ = save_chunks(&path, &known);
    }
}

/// Every thread the store knows, with its display name: contacts, contacts
/// seen only in message traffic, and groups.
async fn known_threads<S: Store>(
    manager: &Manager<S, Registered>,
    data_dir: &Path,
    own_aci: Option<Uuid>,
) -> anyhow::Result<Vec<(Thread, String)>> {
    let mut threads: Vec<(Thread, String)> = Vec::new();
    let mut seen: HashSet<Uuid> = HashSet::new();

    for result in manager.store().contacts().await? {
        let contact = result?;
        seen.insert(contact.uuid);
        let name = if own_aci == Some(contact.uuid) {
            "Note to Self".to_string()
        } else {
            or_uuid(contact_display_name(manager, &contact).await, contact.uuid)
        };
        threads.push((Thread::Contact(ServiceId::Aci(contact.uuid.into())), name));
    }

    for uuid_bytes in load_chunks::<16>(&known_contacts_path(data_dir)) {
        let uuid = Uuid::from_bytes(uuid_bytes);
        if !seen.insert(uuid) {
            continue;
        }
        let name = or_uuid(lookup_contact_name(manager, uuid).await, uuid);
        threads.push((Thread::Contact(ServiceId::Aci(uuid.into())), name));
    }

    for result in manager.store().groups().await? {
        let (master_key, group) = result?;
        threads.push((Thread::Group(master_key), group.title));
    }
    Ok(threads)
}

/// Build a chat list from contacts, known-but-unsynced contacts, and groups.
/// Sorted most-recent-first; threads with no messages are omitted.
pub async fn list_threads<S: Store>(
    manager: &Manager<S, Registered>,
    data_dir: &Path,
    own_aci: Option<Uuid>,
) -> anyhow::Result<Vec<ThreadEntry>> {
    let mut entries: Vec<ThreadEntry> = Vec::new();
    for (thread, name) in known_threads(manager, data_dir, own_aci).await? {
        let (last_preview, last_ts) = last_message(manager, &thread).await;
        if last_ts > 0 {
            entries.push(ThreadEntry::new(thread, name, last_preview, last_ts));
        }
    }
    sort_recent_first(&mut entries);
    Ok(entries)
}

/// A message from someone else, with the chat it was sent in.
pub struct RecentMessage {
    pub ts: u64,
    pub thread: Thread,
    pub thread_name: String,
    pub sender: Uuid,
    pub sender_name: String,
    pub body: String,
    pub mentions_me: bool,
}

/// Messages from other people across every thread since `since_ms`, oldest
/// first, keeping the newest `limit`. `mentions_only` keeps only messages that
/// @-mention this account.
pub async fn recent_messages<S: Store>(
    manager: &mut Manager<S, Registered>,
    data_dir: &Path,
    since_ms: u64,
    mentions_only: bool,
    limit: usize,
) -> anyhow::Result<Vec<RecentMessage>> {
    let own = own_aci(manager);
    let mut recent: Vec<RecentMessage> = Vec::new();
    for (thread, thread_name) in known_threads(manager, data_dir, Some(own)).await? {
        let (messages, _) = load_messages_and_reactions(manager, &thread, Some(since_ms)).await?;
        let from_others: Vec<Content> = messages
            .into_iter()
            .filter(|m| !is_own(m, Some(own)) && (!mentions_only || mentions_account(m, own)))
            .collect();
        if from_others.is_empty() {
            continue;
        }
        let names = names_for_messages(manager, &thread, &from_others).await;
        for message in from_others {
            let sender = message.metadata.sender.raw_uuid();
            recent.push(RecentMessage {
                ts: message.timestamp(),
                thread: thread.clone(),
                thread_name: thread_name.clone(),
                sender,
                sender_name: name_or_uuid(&names, sender),
                body: message_body(&message, |u| name_or_uuid(&names, u)),
                mentions_me: mentions_account(&message, own),
            });
        }
    }
    recent.sort_by_key(|m| m.ts);
    if recent.len() > limit {
        recent.drain(..recent.len() - limit);
    }
    Ok(recent)
}

/// Names for everyone who appears in `messages` as sender or mention: the
/// thread's roster first (members outside the contact store get a cached
/// profile fetch), then the contact store for anyone else.
pub async fn names_for_messages<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    messages: &[Content],
) -> HashMap<Uuid, String> {
    let wanted: HashSet<Uuid> = messages
        .iter()
        .flat_map(|m| std::iter::once(m.metadata.sender.raw_uuid()).chain(mentioned_acis(m)))
        .collect();
    let mut names = load_sender_names(manager, thread, None, Some(&wanted)).await;
    for uuid in wanted {
        if !names.contains_key(&uuid)
            && let Some(name) = lookup_contact_name(manager, uuid).await
        {
            names.insert(uuid, name);
        }
    }
    names
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
        let service_id = ServiceId::Aci(contact.uuid.into());
        let thread = Thread::Contact(service_id);
        let name = or_uuid(contact_display_name(manager, &contact).await, contact.uuid);
        contact_entries.push(ThreadEntry::new(thread, name, None, 0));
    }
    contact_entries.sort_by_key(|e| e.name.to_lowercase());
    let contacts_len = contact_entries.len();

    let mut group_entries: Vec<ThreadEntry> = Vec::new();
    for result in manager.store().groups().await? {
        let (master_key, group) = result?;
        // A group the user has left cannot be messaged; it stays in the chat list for history.
        if let Some(own) = own_aci
            && !is_member(&group, own)
        {
            continue;
        }
        let thread = Thread::Group(master_key);
        group_entries.push(ThreadEntry::new(thread, group.title, None, 0));
    }
    group_entries.sort_by_key(|e| e.name.to_lowercase());

    contact_entries.extend(group_entries);
    Ok((contact_entries, contacts_len))
}

/// True for contacts that have no usable display name — no name field and no phone number.
/// These are candidates for a Signal profile fetch.
fn needs_profile_fetch(contact: &presage::model::contacts::Contact) -> bool {
    contact.name.is_empty() && contact.phone_number.is_none()
}

/// Fetch the Signal profile of every contact without a usable display name.
/// presage caches fetched profiles, which contact_display_name reads.
pub async fn fetch_missing_profiles<S: Store>(
    manager: &mut Manager<S, Registered>,
) -> anyhow::Result<()> {
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

    for contact in nameless {
        let service_id = ServiceId::Aci(contact.uuid.into());
        let Ok(Some(key)) = manager.store().profile_key(&service_id).await else {
            continue;
        };
        if let Err(e) = manager.retrieve_profile_by_uuid(contact.uuid, key).await {
            tracing::warn!("profile fetch failed for {}: {e}", contact.uuid);
        }
    }
    Ok(())
}

/// A contact's name in Signal's order of preference: the address-book name,
/// then the name on their profile (as cached by presage), then phone number.
async fn contact_display_name<S: Store>(
    manager: &Manager<S, Registered>,
    contact: &presage::model::contacts::Contact,
) -> Option<String> {
    if !contact.name.is_empty() {
        return Some(contact.name.clone());
    }
    if let Some(name) = cached_profile_name(manager, contact.uuid).await {
        return Some(name);
    }
    contact.phone_number.as_ref().map(ToString::to_string)
}

async fn cached_profile_name<S: Store>(manager: &Manager<S, Registered>, uuid: Uuid) -> Option<String> {
    let service_id = ServiceId::Aci(uuid.into());
    let key = manager.store().profile_key(&service_id).await.ok().flatten()?;
    let profile = manager.store().profile(uuid, key).await.ok().flatten()?;
    profile_display_name(profile.name)
}

/// A profile's name as display text, if it has one.
fn profile_display_name<N: ToString>(name: Option<N>) -> Option<String> {
    name.map(|n| n.to_string()).filter(|s| !s.is_empty())
}

pub fn or_uuid(name: Option<String>, uuid: Uuid) -> String {
    name.unwrap_or_else(|| uuid.to_string())
}

fn is_member(group: &presage::model::groups::Group, aci: Uuid) -> bool {
    group.members.iter().any(|m| ServiceId::Aci(m.aci).raw_uuid() == aci)
}

// On-disk format for both known contacts (16-byte UUIDs) and known group
// master keys (32-byte blobs): a flat concatenation of fixed-size chunks.
// The chunk size is compile-time via const generic N.
fn load_chunks<const N: usize>(path: &Path) -> HashSet<[u8; N]> {
    std::fs::read(path)
        .unwrap_or_default()
        .chunks_exact(N)
        .filter_map(|c| c.try_into().ok())
        .collect()
}

fn save_chunks<const N: usize>(path: &Path, items: &HashSet<[u8; N]>) -> std::io::Result<()> {
    let bytes: Vec<u8> = items.iter().flat_map(|k| k.iter().copied()).collect();
    std::fs::write(path, bytes)
}

/// Preview text and timestamp of the newest displayable message in a thread.
/// Receipts, reactions, deletions and edit records are skipped so bookkeeping
/// rows cannot reorder the chat list. The store yields newest first, so an edit
/// is seen before its target and replaces the target's text. A thread whose
/// newest rows are all bookkeeping is treated as empty after a bounded scan.
async fn last_message<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
) -> (Option<String>, u64) {
    let Ok(iter) = manager.store().messages(thread, ..).await else {
        return (None, 0);
    };
    let mut edits: HashMap<u64, DataMessage> = HashMap::new();
    for result in iter.take(500) {
        let Ok(mut content) = result else { continue };
        if let Some((target_ts, edited)) = extract_edit(&content) {
            edits.entry(target_ts).or_insert(edited);
            continue;
        }
        if !is_displayable(&content) {
            continue;
        }
        if let Some(edited) = edits.get(&content.timestamp()) {
            apply_edit(&mut content, edited);
        }
        let names = mention_names(manager, &content).await;
        return (Some(message_body(&content, |u| name_or_uuid(&names, u))), content.timestamp());
    }
    (None, 0)
}

/// The DataMessage carried by incoming content or by a sync of our own sent message.
pub(crate) fn data_message_of(content: &Content) -> Option<&DataMessage> {
    match &content.body {
        ContentBody::DataMessage(msg) => Some(msg),
        ContentBody::SynchronizeMessage(SyncMessage {
            sent: Some(Sent { message: Some(msg), .. }),
            ..
        }) => Some(msg),
        _ => None,
    }
}

fn data_message_of_mut(content: &mut Content) -> Option<&mut DataMessage> {
    match &mut content.body {
        ContentBody::DataMessage(msg) => Some(msg),
        ContentBody::SynchronizeMessage(SyncMessage {
            sent: Some(Sent { message: Some(msg), .. }),
            ..
        }) => Some(msg),
        _ => None,
    }
}

/// What a message shows: its text with mention ranges, a label standing in for
/// content the terminal cannot render, or nothing (receipts, reactions, deletes).
enum Shown<'a> {
    Text(&'a str, &'a [BodyRange]),
    Label(Cow<'static, str>),
    Nothing,
}

impl<'a> Shown<'a> {
    fn of_message(msg: &'a DataMessage) -> Self {
        let flags = msg.flags.unwrap_or(0);
        if flags & data_message::Flags::ExpirationTimerUpdate as u32 != 0 {
            return Shown::Label(match msg.expire_timer {
                Some(secs) if secs > 0 => format!("Disappearing message timer set to {}", fmt_duration(secs)).into(),
                _ => "Disappearing messages turned off".into(),
            });
        }
        if flags & data_message::Flags::EndSession as u32 != 0 {
            return Shown::Label("Session reset".into());
        }
        let label = match msg.body.as_deref() {
            Some(text) if !text.is_empty() => return Shown::Text(text, &msg.body_ranges),
            _ if msg.is_view_once == Some(true) => "View-once media",
            _ if !msg.attachments.is_empty() => "Attachment",
            _ if msg.sticker.is_some() => "Sticker",
            _ if msg.payment.is_some() => "Payment",
            _ if msg.gift_badge.is_some() => "Gift badge",
            _ if !msg.contact.is_empty() => "Contact card",
            _ if msg.poll_create.is_some() => "Poll",
            _ if msg.group_v2.as_ref().is_some_and(|g| g.group_change.is_some()) => "Group updated",
            _ => return Shown::Nothing,
        };
        Shown::Label(label.into())
    }

    fn of_content(content: &'a Content) -> Self {
        if let Some(msg) = data_message_of(content) {
            return Shown::of_message(msg);
        }
        match &content.body {
            // Only the offer marks a call; ICE updates, hangups and busy signals are noise.
            ContentBody::CallMessage(call) if call.offer.is_some() => Shown::Label("Incoming call".into()),
            _ => Shown::Nothing,
        }
    }

    /// Mention placeholders become `@Name` via `name_of`.
    fn render(self, name_of: impl Fn(Uuid) -> String) -> String {
        match self {
            Shown::Text(text, ranges) => substitute_mentions(text, ranges, name_of),
            Shown::Label(label) => label.into_owned(),
            Shown::Nothing => String::new(),
        }
    }
}

/// Display text of a message: its text with mentions rendered, or a label for
/// content the terminal cannot show.
pub fn message_body(content: &Content, name_of: impl Fn(Uuid) -> String) -> String {
    Shown::of_content(content).render(name_of)
}

/// Whether the message has anything to show in a chat or preview.
pub fn is_displayable(content: &Content) -> bool {
    !matches!(Shown::of_content(content), Shown::Nothing)
}

/// The thread's disappearing-message timer in seconds, if one is set.
pub async fn expire_timer<S: Store>(manager: &Manager<S, Registered>, thread: &Thread) -> Option<u32> {
    manager.store().expire_timer(thread).await.ok().flatten()
        .map(|(secs, _)| secs)
        .filter(|&secs| secs > 0)
}

/// Human-readable duration for whole weeks, days, hours, minutes or seconds.
pub(crate) fn fmt_duration(secs: u32) -> String {
    let (size, unit) = [(604_800, "week"), (86_400, "day"), (3_600, "hour"), (60, "minute")]
        .into_iter()
        .find(|(size, _)| secs.is_multiple_of(*size))
        .unwrap_or((1, "second"));
    let n = secs / size;
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// The message's text with mentions rendered, or empty when it has no text.
/// Unlike message_body this never yields a label, so it is safe to edit and resend.
pub fn message_text(content: &Content, name_of: impl Fn(Uuid) -> String) -> String {
    match data_message_of(content).map(Shown::of_message) {
        Some(Shown::Text(text, ranges)) => substitute_mentions(text, ranges, name_of),
        _ => String::new(),
    }
}

/// Names for the accounts mentioned in `content`, looked up in the contact store.
pub async fn mention_names<S: Store>(
    manager: &Manager<S, Registered>,
    content: &Content,
) -> HashMap<Uuid, String> {
    let ranges = text_message_of(content).map(|m| m.body_ranges.as_slice()).unwrap_or_default();
    names_for_ranges(manager, ranges).await
}

async fn names_for_ranges<S: Store>(
    manager: &Manager<S, Registered>,
    ranges: &[BodyRange],
) -> HashMap<Uuid, String> {
    let mut names = HashMap::new();
    for (_, _, aci) in mention_ranges(ranges) {
        if let Some(name) = lookup_contact_name(manager, aci).await {
            names.insert(aci, name);
        }
    }
    names
}

/// Display name of a thread: the contact's name, Note to Self, or the group title.
pub async fn thread_title<S: Store>(manager: &Manager<S, Registered>, thread: &Thread) -> String {
    match thread {
        Thread::Contact(sid) if sid.raw_uuid() == own_aci(manager) => "Note to Self".to_string(),
        Thread::Contact(sid) => or_uuid(lookup_contact_name(manager, sid.raw_uuid()).await, sid.raw_uuid()),
        Thread::Group(key) => match manager.store().group(*key).await {
            Ok(Some(group)) => group.title,
            _ => "Unknown group".to_string(),
        },
    }
}

pub fn name_or_uuid(names: &HashMap<Uuid, String>, uuid: Uuid) -> String {
    names.get(&uuid).cloned().unwrap_or_else(|| uuid.to_string())
}

/// The quote a reply to `content` carries: the original's timestamp, author,
/// text with its mention ranges, and the type of any media it had.
pub fn quote_of(content: &Content) -> data_message::Quote {
    let msg = data_message_of(content);
    let media = msg.into_iter().flat_map(|m| {
        m.attachments.iter().chain(m.sticker.as_ref().and_then(|s| s.data.as_ref()))
    });
    let author = content.metadata.sender.raw_uuid();
    data_message::Quote {
        id: Some(content.timestamp()),
        author_aci: Some(author.to_string()),
        author_aci_binary: Some(author.as_bytes().to_vec()),
        text: msg.and_then(|m| m.body.clone()).filter(|b| !b.is_empty()),
        body_ranges: msg.map(|m| m.body_ranges.clone()).unwrap_or_default(),
        attachments: media
            .map(|a| QuotedAttachment {
                content_type: a.content_type.clone(),
                file_name: a.file_name.clone(),
                thumbnail: None,
            })
            .collect(),
        ..Default::default()
    }
}

/// A contact's display name, if the contact store knows them.
pub async fn lookup_contact_name<S: Store>(
    manager: &Manager<S, Registered>,
    uuid: Uuid,
) -> Option<String> {
    let service_id = ServiceId::Aci(uuid.into());
    let contact = manager.store().contact_by_id(&service_id).await.ok().flatten()?;
    contact_display_name(manager, &contact).await
}

/// Names of a thread's participants other than `own_aci`, keyed by ACI, for
/// sender labels, mention rendering and @completion. Group members missing from
/// the contact store get a profile fetch with the roster's profile key (presage
/// caches the result); `wanted` restricts a group to the given members.
/// Members that cannot be resolved are left out, so callers fall back to the UUID.
pub async fn load_sender_names<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    own_aci: Option<Uuid>,
    wanted: Option<&HashSet<Uuid>>,
) -> HashMap<Uuid, String> {
    let mut map: HashMap<Uuid, String> = HashMap::new();
    match thread {
        Thread::Group(key) => {
            let Ok(Some(group)) = manager.store().group(*key).await else {
                return map;
            };
            for member in group.members {
                let uuid = ServiceId::Aci(member.aci).raw_uuid();
                if own_aci == Some(uuid) || wanted.is_some_and(|w| !w.contains(&uuid)) {
                    continue;
                }
                let name = match lookup_contact_name(manager, uuid).await {
                    Some(name) => Some(name),
                    None => match manager.retrieve_profile_by_uuid(uuid, member.profile_key).await {
                        Ok(profile) => profile_display_name(profile.name),
                        Err(e) => {
                            tracing::warn!("profile fetch failed for {uuid}: {e}");
                            None
                        }
                    },
                };
                if let Some(name) = name {
                    map.insert(uuid, name);
                }
            }
        }
        Thread::Contact(service_id) => {
            let uuid = service_id.raw_uuid();
            if own_aci != Some(uuid)
                && let Some(name) = lookup_contact_name(manager, uuid).await
            {
                map.insert(uuid, name);
            }
        }
    }
    map
}

/// The edit carried by incoming content or by a sync of our own edit.
fn edit_of(content: &Content) -> Option<&EditMessage> {
    match &content.body {
        ContentBody::EditMessage(edit) => Some(edit),
        ContentBody::SynchronizeMessage(SyncMessage {
            sent: Some(Sent { edit_message: Some(edit), .. }),
            ..
        }) => Some(edit),
        _ => None,
    }
}

/// (target_sent_timestamp, replacement message) of an edit.
fn extract_edit(content: &Content) -> Option<(u64, DataMessage)> {
    let edit = edit_of(content)?;
    Some((edit.target_sent_timestamp?, edit.data_message.clone()?))
}

/// The DataMessage whose text a content item carries: the message itself, or
/// the replacement text of an edit.
pub(crate) fn text_message_of(content: &Content) -> Option<&DataMessage> {
    data_message_of(content).or_else(|| edit_of(content)?.data_message.as_ref())
}

/// Replace the text of a message with its edited version, mentions included.
fn apply_edit(content: &mut Content, edited: &DataMessage) {
    let Some(msg) = data_message_of_mut(content) else { return };
    msg.body = edited.body.clone();
    msg.body_ranges = edited.body_ranges.clone();
}

/// Single-pass load: splits the thread's message store into displayable messages
/// and a reduced ReactionMap. Avoids two separate full scans when both are needed.
///
/// Reaction toggle semantics: events are processed in chronological order (ASC).
/// The same (sender, emoji, target_ts) triple with remove=true undoes a prior add.
pub async fn load_messages_and_reactions<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
    since: Option<u64>,
) -> anyhow::Result<(Vec<Content>, ReactionMap)> {
    let iter = manager
        .store()
        .messages(thread, since.unwrap_or(0)..)
        .await
        .context("failed to load messages")?;

    let mut messages: Vec<Content> = Vec::new();
    let mut events: Vec<ReactionEvent> = Vec::new();
    // target_sent_timestamp → latest edit. Store returns DESC, so the first
    // EditMessage seen for a given target is the most recent edit.
    let mut edits: HashMap<u64, DataMessage> = HashMap::new();

    for result in iter {
        let Ok(content) = result else { continue };

        // Collect edit messages; they replace the original rather than appearing standalone.
        if let Some((target_ts, edited)) = extract_edit(&content) {
            edits.entry(target_ts).or_insert(edited);
            continue;
        }

        let reaction_opt = data_message_of(&content)
            .and_then(|msg| msg.reaction.as_ref())
            .map(|r| (r, content.metadata.sender.raw_uuid()));

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
        } else if is_displayable(&content) {
            messages.push(content);
        }
    }

    // Store returns DESC; both slices need ASC order.
    messages.reverse();
    events.reverse();

    // Apply edits in-place: replace the body of the original message.
    for msg in &mut messages {
        if let Some(edited) = edits.get(&msg.timestamp()) {
            apply_edit(msg, edited);
        }
    }

    Ok((messages, fold_reactions(events)))
}

/// A reaction event: (event_ts, reactor, emoji, remove, target_ts).
type ReactionEvent = (u64, [u8; 16], String, bool, u64);

/// Fold chronologically ordered reaction events into the current state. An
/// account has one reaction per message, so a new emoji replaces that account's
/// previous one; a removal only clears the emoji it names.
fn fold_reactions(events: impl IntoIterator<Item = ReactionEvent>) -> ReactionMap {
    let mut reactions: ReactionMap = HashMap::new();
    for (_, reactor, emoji, remove, target_ts) in events {
        let per_target = reactions.entry(target_ts).or_default();
        if remove {
            if let Some(reactors) = per_target.get_mut(&emoji) {
                reactors.remove(&reactor);
            }
        } else {
            for reactors in per_target.values_mut() {
                reactors.remove(&reactor);
            }
            per_target.entry(emoji).or_default().insert(reactor);
        }
    }
    for emoji_map in reactions.values_mut() {
        emoji_map.retain(|_, reactors| !reactors.is_empty());
    }
    reactions.retain(|_, emoji_map| !emoji_map.is_empty());
    reactions
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
    };
    let data_message = DataMessage {
        reaction: Some(reaction),
        timestamp: Some(ts),
        ..Default::default()
    };
    dispatch_send(manager, thread, data_message, ts).await
}

/// Signal accepts edits and delete-for-everyone for 24 hours after sending.
pub fn outside_edit_window(sent_ts: u64) -> bool {
    const WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
    now_millis().is_ok_and(|now| now.saturating_sub(sent_ts) > WINDOW_MS)
}

/// Replace the text of `original`. An edit carries the whole message again, so
/// the quote and attachments are copied; presage fills the thread's expire
/// timer for DataMessage bodies only, so it is copied here as well.
pub async fn send_edit<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    original: &Content,
    body: String,
    body_ranges: Vec<BodyRange>,
) -> anyhow::Result<()> {
    let ts = now_millis()?;
    let previous = data_message_of(original);
    let timer = manager.store().expire_timer(thread).await.ok().flatten();
    let data_message = DataMessage {
        body: Some(body),
        body_ranges,
        timestamp: Some(ts),
        quote: previous.and_then(|m| m.quote.clone()),
        attachments: previous.map(|m| m.attachments.clone()).unwrap_or_default(),
        expire_timer: timer.and_then(|(secs, _)| (secs > 0).then_some(secs)),
        expire_timer_version: Some(timer.map_or(0, |(_, version)| version)),
        ..Default::default()
    };
    let edit = ContentBody::EditMessage(EditMessage {
        target_sent_timestamp: Some(original.timestamp()),
        data_message: Some(data_message),
    });
    dispatch_send_body(manager, thread, edit, ts).await
}

/// Send a message; returns its timestamp.
pub async fn send_to_thread<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    text: String,
    body_ranges: Vec<BodyRange>,
    attachments: Vec<AttachmentPointer>,
) -> anyhow::Result<u64> {
    let ts = now_millis()?;
    let data_message = DataMessage {
        body: if text.is_empty() { None } else { Some(text) },
        body_ranges,
        timestamp: Some(ts),
        attachments,
        ..Default::default()
    };
    dispatch_send(manager, thread, data_message, ts).await?;
    Ok(ts)
}

/// Send text and files to a thread, with `@Name` mentions resolved against the
/// thread's roster. Returns the message timestamp.
pub async fn send_text<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    text: &str,
    attachments: &[PathBuf],
) -> anyhow::Result<u64> {
    if text.is_empty() && attachments.is_empty() {
        anyhow::bail!("nothing to send (no text and no attachments)");
    }
    let staged = attachments
        .iter()
        .map(|path| stage_attachment(path).map_err(|e| anyhow::anyhow!(e)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let pointers = if staged.is_empty() {
        Vec::new()
    } else {
        upload_staged_attachments(manager, &staged).await.map_err(|(msg, _)| anyhow::anyhow!(msg))?
    };
    // Mentions need the roster; skip those lookups when there is nothing to match.
    let (body, body_ranges) = if text.contains('@') {
        let names = load_sender_names(manager, thread, Some(own_aci(manager)), None).await;
        encode_mentions(text, &names)
    } else {
        (text.to_string(), Vec::new())
    };
    send_to_thread(manager, thread, body, body_ranges, pointers).await
}

pub async fn send_reply<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    text: String,
    body_ranges: Vec<BodyRange>,
    quote: data_message::Quote,
    attachments: Vec<AttachmentPointer>,
) -> anyhow::Result<()> {
    let ts = now_millis()?;
    let data_message = DataMessage {
        body: if text.is_empty() { None } else { Some(text) },
        body_ranges,
        timestamp: Some(ts),
        quote: Some(quote),
        attachments,
        ..Default::default()
    };
    dispatch_send(manager, thread, data_message, ts).await
}

/// Author and text of the message a reply quotes, mentions rendered via `name_of`.
pub fn message_quote(content: &Content, name_of: impl Fn(Uuid) -> String) -> Option<(Uuid, String)> {
    let quote = data_message_of(content)?.quote.as_ref()?;
    // Prefer the binary ACI field; fall back to the string form.
    let author = if let Some(bytes) = &quote.author_aci_binary {
        Uuid::from_slice(bytes).ok()?
    } else {
        Uuid::parse_str(quote.author_aci.as_deref()?).ok()?
    };
    let shown = match quote.text.as_deref() {
        Some(text) if !text.is_empty() => Shown::Text(text, &quote.body_ranges),
        _ if !quote.attachments.is_empty() => Shown::Label("Attachment".into()),
        _ => Shown::Nothing,
    };
    Some((author, shown.render(name_of)))
}

pub fn own_service_id<S: Store>(manager: &Manager<S, Registered>) -> ServiceId {
    ServiceId::Aci(manager.registration_data().service_ids.aci())
}

pub fn own_aci<S: Store>(manager: &Manager<S, Registered>) -> Uuid {
    own_service_id(manager).raw_uuid()
}

/// Add a receipt's timestamps to the delivery state; read implies delivered.
pub(crate) fn fold_receipt(receipt: &ReceiptMessage, delivered: &mut HashSet<u64>, read: &mut HashSet<u64>) {
    match receipt.r#type.and_then(|t| receipt_message::Type::try_from(t).ok()) {
        Some(receipt_message::Type::Delivery) => delivered.extend(receipt.timestamp.iter().copied()),
        Some(receipt_message::Type::Read) => {
            read.extend(receipt.timestamp.iter().copied());
            delivered.extend(receipt.timestamp.iter().copied());
        }
        _ => {}
    }
}

/// Delivery and read receipts for our messages among `messages`, which belong
/// to `thread`, as (delivered_timestamps, read_timestamps). Group members send
/// receipts in their own 1:1 thread, so a group is resolved by scanning every
/// member's thread, from our oldest message onward.
pub async fn load_receipt_state<S: Store>(
    manager: &Manager<S, Registered>,
    thread: &Thread,
    messages: &[Content],
) -> anyhow::Result<(HashSet<u64>, HashSet<u64>)> {
    let own = own_aci(manager);
    let mut delivered: HashSet<u64> = HashSet::new();
    let mut read: HashSet<u64> = HashSet::new();
    let oldest_own = messages.iter().filter(|m| is_own(m, Some(own))).map(|m| m.timestamp()).min();
    let Some(from) = oldest_own else {
        return Ok((delivered, read));
    };
    let sources: Vec<Thread> = match thread {
        Thread::Contact(_) => vec![thread.clone()],
        Thread::Group(key) => match manager.store().group(*key).await {
            Ok(Some(group)) => group.members.into_iter().map(|m| Thread::Contact(ServiceId::Aci(m.aci))).collect(),
            _ => Vec::new(),
        },
    };
    for source in &sources {
        let iter = manager
            .store()
            .messages(source, from..)
            .await
            .context("failed to load messages for receipt state")?;
        for result in iter {
            let Ok(content) = result else { continue };
            // Receipts we sent are stored in the same thread; they say nothing about our messages.
            if is_own(&content, Some(own)) {
                continue;
            }
            if let ContentBody::ReceiptMessage(receipt) = &content.body {
                fold_receipt(receipt, &mut delivered, &mut read);
            }
        }
    }
    Ok((delivered, read))
}

/// Tell each author we read their messages, then tell our other devices so they
/// clear the unread state too. `read` pairs each message's author with its timestamp.
pub async fn send_read_receipts<S: Store>(
    manager: &mut Manager<S, Registered>,
    read: Vec<(Uuid, u64)>,
) -> anyhow::Result<()> {
    if read.is_empty() {
        return Ok(());
    }
    let mut by_author: HashMap<Uuid, Vec<u64>> = HashMap::new();
    for (author, ts) in &read {
        by_author.entry(*author).or_default().push(*ts);
    }
    for (author, timestamps) in by_author {
        let receipt = ReceiptMessage {
            r#type: Some(receipt_message::Type::Read as i32),
            timestamp: timestamps,
        };
        manager
            .send_message(ServiceId::Aci(author.into()), receipt, now_millis()?)
            .await
            .context("failed to send read receipt")?;
    }
    let sync = SyncMessage {
        read: read
            .into_iter()
            .map(|(author, ts)| sync_message::Read {
                sender_aci: Some(author.to_string()),
                sender_aci_binary: Some(author.as_bytes().to_vec()),
                timestamp: Some(ts),
            })
            .collect(),
        ..Default::default()
    };
    manager
        .send_message(own_service_id(manager), sync, now_millis()?)
        .await
        .context("failed to sync read state to own devices")?;
    Ok(())
}

// u128 → u64 truncation only matters past year ~584 million.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn now_millis() -> anyhow::Result<u64> {
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
        let group = manager.store().group(*master_key).await.ok().flatten();
        if let Some(group) = &group
            && !is_member(group, own_aci(manager))
        {
            anyhow::bail!("you are no longer a member of this group");
        }
        let ctx = GroupContextV2 {
            master_key: Some(master_key.to_vec()),
            revision: Some(group.map_or(0, |g| g.revision)),
            group_change: None,
        };
        // presage attaches our profile key to 1:1 messages only; group messages
        // carry it too, as the official clients do.
        let profile_key = manager.registration_data().profile_key().get_bytes().to_vec();
        match &mut body {
            ContentBody::DataMessage(dm) => {
                dm.group_v2 = Some(ctx);
                dm.profile_key.get_or_insert(profile_key);
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
                .send_message(*service_id, body, ts)
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

// Signal timestamps (unix ms) wrap i64 only past year ~292 billion.
#[allow(clippy::cast_possible_wrap)]
pub(crate) fn ts_to_datetime(ts_ms: u64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp((ts_ms / 1000) as i64, 0)
}

pub(crate) fn fmt_ts_short(ts_ms: u64) -> String {
    ts_to_datetime(ts_ms)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

pub(crate) fn fmt_ts_long(ts_ms: u64) -> String {
    ts_to_datetime(ts_ms)
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> Uuid {
        Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
    }

    fn bob() -> Uuid {
        Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
    }

    #[test]
    fn new_reaction_replaces_the_reactors_previous_one() {
        let a = *alice().as_bytes();
        let folded = fold_reactions([
            (1, a, "👍".to_string(), false, 100),
            (2, a, "❤️".to_string(), false, 100),
        ]);
        let per_target = &folded[&100];
        assert!(!per_target.contains_key("👍"));
        assert!(per_target["❤️"].contains(&a));
    }

    #[test]
    fn removal_clears_only_the_named_emoji_for_that_reactor() {
        let (a, b) = (*alice().as_bytes(), *bob().as_bytes());
        let folded = fold_reactions([
            (1, a, "❤️".to_string(), false, 100),
            (2, b, "❤️".to_string(), false, 100),
            (3, a, "👍".to_string(), true, 100),
            (4, a, "❤️".to_string(), true, 100),
        ]);
        assert_eq!(folded[&100]["❤️"].len(), 1);
        assert!(folded[&100]["❤️"].contains(&b));
        let gone = fold_reactions([
            (1, a, "❤️".to_string(), false, 100),
            (2, a, "❤️".to_string(), true, 100),
        ]);
        assert!(gone.is_empty());
    }

    #[test]
    fn durations_use_the_largest_whole_unit() {
        assert_eq!(fmt_duration(604_800), "1 week");
        assert_eq!(fmt_duration(2 * 86_400), "2 days");
        assert_eq!(fmt_duration(3_600), "1 hour");
        assert_eq!(fmt_duration(300), "5 minutes");
        assert_eq!(fmt_duration(30), "30 seconds");
        assert_eq!(fmt_duration(90), "90 seconds");
    }

    #[test]
    fn thread_ids_parse_as_contact_or_group() {
        assert!(matches!(parse_thread_id("11111111-1111-1111-1111-111111111111"), Ok(Thread::Contact(_))));
        assert!(matches!(parse_thread_id(&"ab".repeat(32)), Ok(Thread::Group(_))));
        assert!(parse_thread_id("not-a-recipient").is_err());
        assert!(parse_thread_id(&"zz".repeat(32)).is_err());
    }
}
