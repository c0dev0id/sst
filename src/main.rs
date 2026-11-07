mod app;
mod attachments;
mod ipc;
mod mentions;
mod signal;
mod ui;

use anyhow::Context as _;
use clap::{Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;
use futures::{StreamExt, channel::oneshot, future};
use presage::libsignal_service::configuration::SignalServers;
use presage::libsignal_service::content::Content;
use presage::manager::Registered;
use presage::model::identity::OnNewIdentity;
use presage::libsignal_service::prelude::Uuid;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::Manager;
use presage_store_sqlite::SqliteStore;
use std::path::{Path, PathBuf};
use tracing::error;

// OpenBSD default stack limit is 4MB which is insufficient for the PQ ratchet
// crypto stack frames inside LocalSet (futures run on the calling thread).
// Spawn the runtime on a thread with a generous stack instead.
const STACK_SIZE: usize = 64 * 1024 * 1024;

#[derive(Clone, ValueEnum)]
enum Format {
    Text,
    Json,
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Format::Text => write!(f, "text"),
            Format::Json => write!(f, "json"),
        }
    }
}

#[derive(Parser)]
#[clap(about = "Signal TUI client")]
struct Args {
    #[clap(long, help = "Path to the SQLite database (default: XDG data dir)")]
    db: Option<PathBuf>,

    #[clap(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Link this device via QR code; wipes existing session if already registered
    Link,

    /// List chats sorted by most recent activity
    Chats {
        #[clap(long, value_enum, default_value_t = Format::Text, value_name = "FORMAT")]
        format: Format,
    },

    /// List all contacts and groups
    Contacts {
        #[clap(long, value_enum, default_value_t = Format::Text, value_name = "FORMAT")]
        format: Format,
    },

    /// Print full chat history
    Print {
        #[clap(long, value_enum, default_value_t = Format::Text, value_name = "FORMAT")]
        format: Format,

        /// Contact UUID or 64-char group hex key
        #[clap(value_name = "UUID|HEX")]
        recipient: String,
    },

    /// Print the last N messages (default: 1)
    PrintLast {
        /// Number of messages to print
        #[clap(short = 'n', default_value = "1", value_name = "N")]
        count: usize,

        #[clap(long, value_enum, default_value_t = Format::Text, value_name = "FORMAT")]
        format: Format,

        /// Contact UUID or 64-char group hex key
        #[clap(value_name = "UUID|HEX")]
        recipient: String,
    },

    /// List messages from others across all chats since a point in time
    Recent {
        /// RFC 3339 timestamp, or a duration back from now like 30m, 2h, 7d
        #[clap(long, default_value = "24h", value_name = "WHEN")]
        since: String,

        /// Only messages that @-mention this account
        #[clap(long)]
        mentions_me: bool,

        /// Keep only the newest N messages
        #[clap(short = 'n', long, default_value = "200", value_name = "N")]
        limit: usize,

        #[clap(long, value_enum, default_value_t = Format::Text, value_name = "FORMAT")]
        format: Format,
    },

    /// Stream incoming messages (runs until interrupted)
    Watch {
        #[clap(long, value_enum, default_value_t = Format::Text, value_name = "FORMAT")]
        format: Format,

        /// Contact UUID or 64-char group hex key
        #[clap(value_name = "UUID|HEX")]
        recipient: String,
    },

    /// Send a message; reads text from stdin if no message argument given
    Send {
        /// Contact UUID or 64-char group hex key
        #[clap(value_name = "UUID|HEX")]
        recipient: String,

        /// Message text; reads from stdin if omitted
        text: Option<String>,

        /// Attach a file (can be repeated)
        #[clap(long, value_name = "PATH")]
        attach: Vec<PathBuf>,
    },
}

impl Cmd {
    /// Subcommands that read the store as is when another instance holds the
    /// session, and sync first only when the lock is free.
    fn can_run_beside_another_instance(&self) -> bool {
        matches!(
            self,
            Cmd::Chats { .. } | Cmd::Contacts { .. } | Cmd::Print { .. } | Cmd::PrintLast { .. } | Cmd::Recent { .. }
        )
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime")
                .block_on(async_main(args))
        })?
        .join()
        .expect("main thread panicked")
}

async fn async_main(args: Args) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => ProjectDirs::from("", "", "sst")
            .context("could not determine data directory (is $HOME set?)")?
            .data_dir()
            .join("db"),
    };
    let data_dir = db_path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&data_dir)?;

    let outcome = match args.cmd {
        Some(Cmd::Send { recipient, text, attach }) => {
            let request = ipc::SendRequest::new(recipient, send_text_from(text)?, attach)?;
            send(request, db_path, data_dir).await
        }
        cmd => run_locked(cmd, db_path, data_dir).await,
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Some(hint) = signal::relink_hint(&e) {
                eprintln!("{hint}");
                eprintln!("(underlying error: {e})");
                std::process::exit(1);
            }
            Err(e)
        }
    }
}

/// The message text for `sst send`: the argument, or stdin when it is omitted.
fn send_text_from(arg: Option<String>) -> anyhow::Result<String> {
    if let Some(text) = arg {
        return Ok(text);
    }
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

fn open_lock_file(data_dir: &Path) -> anyhow::Result<fd_lock::RwLock<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join("sst.lock"))
        .context("failed to open lock file")?;
    Ok(fd_lock::RwLock::new(file))
}

/// Run a subcommand that uses the store directly. The TUI, watch and link
/// need the Signal session to themselves and fail if another instance holds
/// the lock; the others degrade to reading the store (see Cmd).
async fn run_locked(cmd: Option<Cmd>, db_path: PathBuf, data_dir: PathBuf) -> anyhow::Result<()> {
    let can_share = cmd.as_ref().is_some_and(Cmd::can_run_beside_another_instance);
    let mut lock = open_lock_file(&data_dir)?;
    let lock_guard = match lock.try_write() {
        Ok(guard) => Some(guard),
        Err(_) if can_share => None,
        Err(_) => anyhow::bail!(
            "another sst instance is already running\n\
             (only one instance may use the Signal session at a time)"
        ),
    };
    run_inner(cmd, db_path, data_dir, lock_guard.is_some()).await
}

/// How long `sst send` keeps trying while another instance holds the session
/// without serving the send socket yet (starting up, or a short-lived subcommand).
const SEND_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Hand the message to the running session holder over the send socket, or,
/// when no instance is running, take the lock and send directly.
async fn send(request: ipc::SendRequest, db_path: PathBuf, data_dir: PathBuf) -> anyhow::Result<()> {
    let socket = ipc::socket_path(&data_dir);
    let mut lock = open_lock_file(&data_dir)?;
    let deadline = std::time::Instant::now() + SEND_WAIT;
    loop {
        if let Some(outcome) = ipc::request_send(&socket, &request)? {
            return outcome.map_err(anyhow::Error::msg);
        }
        if let Ok(_guard) = lock.try_write() {
            return send_direct(request, db_path, data_dir).await;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("another sst instance holds the Signal session but is not accepting sends");
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn send_direct(request: ipc::SendRequest, db_path: PathBuf, data_dir: PathBuf) -> anyhow::Result<()> {
    let store = open_store(&db_path, &data_dir).await?;
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut manager = load_manager(store).await?;
            ipc::perform(&mut manager, &request).await?;
            Ok(())
        })
        .await
}

/// `may_sync`: whether this process holds the instance lock and may therefore
/// drain the message queue before reading the store.
async fn run_inner(cmd: Option<Cmd>, db_path: PathBuf, data_dir: PathBuf, may_sync: bool) -> anyhow::Result<()> {
    if matches!(cmd, Some(Cmd::Link)) {
        if db_path.exists() {
            eprintln!("Warning: this will wipe the local database at {}", db_path.display());
            eprintln!("All locally stored messages and contacts will be lost.");
            eprint!("Continue? [y/N] ");
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                anyhow::bail!("aborted");
            }
        }
        let base = db_path.file_name().unwrap().to_string_lossy().into_owned();
        for suffix in &["", "-wal", "-shm"] {
            let p = db_path.with_file_name(format!("{base}{suffix}"));
            std::fs::remove_file(&p).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
            }).with_context(|| format!("failed to remove {}", p.display()))?;
        }
        eprintln!("Database wiped. Linking new device…");
    }

    let store = open_store(&db_path, &data_dir).await?;
    let local = tokio::task::LocalSet::new();
    local.run_until(run(cmd, store, data_dir, may_sync)).await
}

/// Start logging into the data directory and open the store.
async fn open_store(db_path: &Path, data_dir: &Path) -> anyhow::Result<SqliteStore> {
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("sst.log"))?;
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing::metadata::LevelFilter::WARN.into())
        .from_env_lossy()
        .add_directive("libsignal=error".parse().unwrap());
    tracing_subscriber::fmt::fmt()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_env_filter(filter)
        .init();

    SqliteStore::open_with_passphrase(
        db_path.to_str().context("non-UTF-8 db path")?,
        None::<&str>,
        OnNewIdentity::Trust,
    )
    .await
    .context("failed to open store")
}

async fn load_manager<S: Store>(store: S) -> anyhow::Result<Manager<S, Registered>> {
    Manager::load_registered(store)
        .await
        .map_err(|_| anyhow::anyhow!("No existing registration found. Run `sst link` to link this device."))
}

async fn run<S: Store>(
    cmd: Option<Cmd>,
    store: S,
    data_dir: PathBuf,
    may_sync: bool,
) -> anyhow::Result<()> {
    let mut manager = if matches!(cmd, Some(Cmd::Link)) {
        link_device(store).await?
    } else {
        load_manager(store).await?
    };

    let own = Some(signal::own_aci(&manager));

    match cmd {
        None => {
            let stream = signal::connect(&mut manager, &data_dir).await?;
            eprintln!("Loading chats…");
            let threads = signal::list_threads(&manager, &data_dir, own, &signal::LastSeen::load(&data_dir)).await?;
            app::run(threads, own, data_dir, manager, stream).await
        }

        Some(Cmd::Link) => {
            eprintln!("Linking successful. Device provisioned.");
            Ok(())
        }

        Some(Cmd::Chats { format }) => {
            if may_sync {
                signal::sync(&mut manager, &data_dir).await?;
            }
            let threads = signal::list_threads(&manager, &data_dir, own, &signal::LastSeen::load(&data_dir)).await?;
            for entry in &threads {
                match format {
                    Format::Text => {
                        match entry.last_preview.as_deref() {
                            Some(p) if !p.is_empty() => println!("{}: {}", entry.name, p),
                            _ => println!("{}", entry.name),
                        }
                    }
                    Format::Json => println!("{}", chat_record(entry)),
                }
            }
            Ok(())
        }

        Some(Cmd::Contacts { format }) => {
            if may_sync {
                eprintln!("Syncing contacts…");
                signal::sync_contacts(&mut manager).await?;
                if let Err(e) = signal::fetch_missing_profiles(&mut manager).await {
                    tracing::warn!("profile fetch: {e:#}");
                }
            }
            let (contacts, _) = signal::list_all_contacts(&manager, own).await?;
            for entry in &contacts {
                match format {
                    Format::Text => println!("{} {}", signal::thread_id(&entry.thread), entry.name),
                    Format::Json => println!("{}", contact_record(entry)),
                }
            }
            Ok(())
        }

        Some(Cmd::Print { format, recipient }) => {
            if may_sync {
                signal::sync(&mut manager, &data_dir).await?;
            }
            let thread = signal::parse_thread_id(&recipient)?;
            let (messages, _) = signal::load_messages_and_reactions(&manager, &thread, None).await?;
            print_messages(&mut manager, &thread, &messages, &format).await;
            Ok(())
        }

        Some(Cmd::PrintLast { count, format, recipient }) => {
            if may_sync {
                signal::sync(&mut manager, &data_dir).await?;
            }
            let thread = signal::parse_thread_id(&recipient)?;
            let (messages, _) = signal::load_messages_and_reactions(&manager, &thread, None).await?;
            let start = messages.len().saturating_sub(count);
            print_messages(&mut manager, &thread, &messages[start..], &format).await;
            Ok(())
        }

        Some(Cmd::Recent { since, mentions_me, limit, format }) => {
            if may_sync {
                signal::sync(&mut manager, &data_dir).await?;
            }
            let since_ms = since_to_millis(&since, signal::now_millis()?)?;
            let recent = signal::recent_messages(&mut manager, &data_dir, since_ms, mentions_me, limit).await?;
            for m in &recent {
                match format {
                    Format::Text => {
                        let mark = if m.mentions_me { "@ " } else { "" };
                        println!("[{}] {}{} / {}: {}", signal::fmt_ts_long(m.ts), mark, m.thread_name, m.sender_name, m.body);
                    }
                    Format::Json => println!("{}", recent_record(m)),
                }
            }
            Ok(())
        }

        Some(Cmd::Watch { format, recipient }) => {
            let thread = signal::parse_thread_id(&recipient)?;
            let (mut send_jobs, _send_socket) = ipc::serve(&data_dir)?;
            // BTreeSet allows evicting entries older than the dedup window in O(log n).
            let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
            let mut names = signal::load_sender_names(&mut manager, &thread, None, None).await;
            let start_ts = signal::now_millis().unwrap_or(0);

            'reconnect: loop {
                let mut stream = Box::pin(match manager.receive_messages().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("receive_messages failed: {e}, retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue 'reconnect;
                    }
                });

                loop {
                    tokio::select! {
                        Some(job) = send_jobs.recv() => {
                            let result = ipc::perform(&mut manager, &job.request).await;
                            job.finish(result);
                        }
                        event = stream.next() => {
                            let Some(event) = event else { break };
                            let Received::Content(boxed) = event else { continue };
                            cache_mentioned_names(&mut names, &manager, &boxed).await;
                            let Some(update) = signal::extract_update(&boxed, |u| signal::name_or_uuid(&names, u)) else {
                                continue;
                            };
                            if update.thread != thread || update.ts < start_ts { continue; }
                            // Evict timestamps older than 60 s to keep the dedup set bounded.
                            let horizon = update.ts.saturating_sub(60_000);
                            while seen.first().is_some_and(|&t| t < horizon) {
                                seen.pop_first();
                            }
                            if !seen.insert(update.ts) { continue; }
                            let sender_uuid = boxed.metadata.sender.raw_uuid();
                            let sender_name = cached_sender_name(&mut names, &manager, sender_uuid).await;
                            let line = format_one(&format, update.ts, sender_uuid, &sender_name, &update.preview, update.edit_of);
                            println!("{}", line);
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        }
                    }
                }

                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        Some(Cmd::Send { .. }) => unreachable!("send is handled before the store is opened"),
    }
}

// ── Output helpers ────────────────────────────────────────────────────────────

async fn cached_sender_name<S: Store>(
    cache: &mut std::collections::HashMap<Uuid, String>,
    manager: &Manager<S, Registered>,
    uuid: Uuid,
) -> String {
    if let Some(name) = cache.get(&uuid) {
        return name.clone();
    }
    let name = signal::or_uuid(signal::lookup_contact_name(manager, uuid).await, uuid);
    cache.insert(uuid, name.clone());
    name
}

/// Make sure every account mentioned in `content` has a name in the cache.
async fn cache_mentioned_names<S: Store>(
    cache: &mut std::collections::HashMap<Uuid, String>,
    manager: &Manager<S, Registered>,
    content: &Content,
) {
    for aci in mentions::mentioned_acis(content) {
        cached_sender_name(cache, manager, aci).await;
    }
}

async fn print_messages<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    messages: &[Content],
    format: &Format,
) {
    let names = signal::names_for_messages(manager, thread, messages).await;
    for msg in messages {
        let uuid = msg.metadata.sender.raw_uuid();
        let body = signal::message_body(msg, |u| signal::name_or_uuid(&names, u));
        let line = format_one(format, msg.timestamp(), uuid, &signal::name_or_uuid(&names, uuid), &body, None);
        println!("{line}");
    }
}

fn iso_timestamp(ts_ms: u64) -> String {
    signal::ts_to_datetime(ts_ms).map_or_else(|| ts_ms.to_string(), |dt| dt.to_rfc3339())
}

/// A `--since` value as unix milliseconds: an RFC 3339 timestamp, or a
/// duration back from `now_ms` such as `30m`, `2h` or `7d`.
fn since_to_millis(spec: &str, now_ms: u64) -> anyhow::Result<u64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(spec) {
        return u64::try_from(dt.timestamp_millis()).context("timestamp before 1970");
    }
    let invalid = || anyhow::anyhow!("invalid --since '{spec}': use an RFC 3339 timestamp or a duration like 30m, 2h, 7d");
    let unit = spec.chars().last().ok_or_else(invalid)?;
    let amount: u64 = spec[..spec.len() - unit.len_utf8()].parse().map_err(|_| invalid())?;
    let unit_ms: u64 = match unit {
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return Err(invalid()),
    };
    Ok(now_ms.saturating_sub(amount.saturating_mul(unit_ms)))
}

// The JSON records are an interface (mcpd parses them): one builder per record,
// key sets pinned by tests and listed in the README.

fn chat_record(entry: &signal::ThreadEntry) -> serde_json::Value {
    serde_json::json!({
        "id":        signal::thread_id(&entry.thread),
        "name":      entry.name,
        "preview":   entry.last_preview,
        "timestamp": iso_timestamp(entry.last_ts),
        "unread":    entry.unread,
    })
}

fn contact_record(entry: &signal::ThreadEntry) -> serde_json::Value {
    let kind = match entry.thread {
        Thread::Contact(_) => "contact",
        Thread::Group(_) => "group",
    };
    serde_json::json!({ "id": signal::thread_id(&entry.thread), "name": entry.name, "type": kind })
}

fn recent_record(m: &signal::RecentMessage) -> serde_json::Value {
    let mut record = message_record(m.ts, m.sender, &m.sender_name, &m.body, None);
    record["chat_id"] = serde_json::Value::String(signal::thread_id(&m.thread));
    record["chat_name"] = serde_json::Value::String(m.thread_name.clone());
    record["mentions_me"] = serde_json::Value::Bool(m.mentions_me);
    record
}

/// The JSON fields every message-printing subcommand shares.
fn message_record(ts_ms: u64, sender_uuid: Uuid, sender_name: &str, body: &str, edit_of: Option<u64>) -> serde_json::Value {
    let mut record = serde_json::json!({
        "timestamp":   iso_timestamp(ts_ms),
        "sender_uuid": sender_uuid.to_string(),
        "sender_name": sender_name,
        "body":        body,
    });
    if let Some(target) = edit_of {
        record["edit_of"] = serde_json::Value::String(iso_timestamp(target));
    }
    record
}

fn format_one(
    format: &Format,
    ts_ms: u64,
    sender_uuid: Uuid,
    sender_name: &str,
    body: &str,
    edit_of: Option<u64>,
) -> String {
    match format {
        Format::Text => {
            let edited = if edit_of.is_some() { " (edited)" } else { "" };
            format!("[{}] {}: {}{}", signal::fmt_ts_long(ts_ms), sender_name, body, edited)
        }
        Format::Json => message_record(ts_ms, sender_uuid, sender_name, body, edit_of).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(record: &serde_json::Value) -> Vec<&str> {
        record.as_object().unwrap().keys().map(String::as_str).collect()
    }

    #[test]
    fn json_records_keep_their_fields() {
        let thread = signal::parse_thread_id("11111111-1111-1111-1111-111111111111").unwrap();
        let entry = signal::ThreadEntry::new(thread.clone(), "Alice".to_string(), Some("hi".to_string()), 1_000);
        assert_eq!(keys(&chat_record(&entry)), ["id", "name", "preview", "timestamp", "unread"]);
        assert_eq!(keys(&contact_record(&entry)), ["id", "name", "type"]);
        assert_eq!(contact_record(&entry)["type"], "contact");
        assert_eq!(
            keys(&message_record(1_000, Uuid::nil(), "Alice", "hi", None)),
            ["body", "sender_name", "sender_uuid", "timestamp"]
        );
        assert_eq!(
            keys(&message_record(1_000, Uuid::nil(), "Alice", "hi", Some(500))),
            ["body", "edit_of", "sender_name", "sender_uuid", "timestamp"]
        );
        let recent = signal::RecentMessage {
            ts: 1_000,
            thread,
            thread_name: "Alice".to_string(),
            sender: Uuid::nil(),
            sender_name: "Alice".to_string(),
            body: "hi".to_string(),
            mentions_me: true,
        };
        assert_eq!(
            keys(&recent_record(&recent)),
            ["body", "chat_id", "chat_name", "mentions_me", "sender_name", "sender_uuid", "timestamp"]
        );
    }

    #[test]
    fn since_accepts_durations_and_timestamps() {
        let now = 1_000_000_000_000;
        assert_eq!(since_to_millis("30m", now).unwrap(), now - 30 * 60_000);
        assert_eq!(since_to_millis("2h", now).unwrap(), now - 2 * 3_600_000);
        assert_eq!(since_to_millis("7d", now).unwrap(), now - 7 * 86_400_000);
        assert_eq!(since_to_millis("1970-01-01T00:00:01+00:00", now).unwrap(), 1_000);
        for bad in ["", "soon", "2x", "h", "2026-11-04"] {
            assert!(since_to_millis(bad, now).is_err(), "{bad}");
        }
    }
}

// ── Device linking ────────────────────────────────────────────────────────────

const LINK_TIMEOUT_SECS: u64 = 300;

async fn link_device<S: Store>(store: S) -> anyhow::Result<Manager<S, Registered>> {
    let (tx, rx) = oneshot::channel();

    let link_fut = Manager::link_secondary_device(
        store,
        SignalServers::Production,
        "sst".to_string(),
        tx,
    );
    let qr_fut = async move {
        match rx.await {
            Ok(url) => {
                eprintln!("Scan this QR code with your Signal app (Settings → Linked devices):");
                qr2term::print_qr(url.to_string()).unwrap_or_else(|e| {
                    eprintln!("QR render failed: {e}");
                    eprintln!("URL: {url}");
                });
                eprintln!("Waiting for scan (up to {} minutes)…", LINK_TIMEOUT_SECS / 60);
            }
            Err(e) => error!("provisioning cancelled: {e}"),
        }
    };

    let joined = future::join(link_fut, qr_fut);
    match tokio::time::timeout(std::time::Duration::from_secs(LINK_TIMEOUT_SECS), joined).await {
        Ok((Ok(manager), _)) => Ok(manager),
        Ok((Err(e), _)) => Err(anyhow::Error::new(e).context("device linking failed")),
        Err(_) => anyhow::bail!(
            "Linking timed out after {} seconds. Scan the QR within the window and rerun `sst link`.",
            LINK_TIMEOUT_SECS
        ),
    }
}
