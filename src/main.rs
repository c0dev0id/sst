mod app;
mod ipc;
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

/// Run a subcommand that uses the store directly. Interactive and streaming
/// subcommands need the Signal session to themselves and fail if another
/// instance holds the lock. The listing subcommands take the lock when it is
/// free so they can sync first; when another instance holds it, that instance
/// keeps the store current and they read it as is.
async fn run_locked(cmd: Option<Cmd>, db_path: PathBuf, data_dir: PathBuf) -> anyhow::Result<()> {
    let read_only_is_fine = matches!(
        cmd,
        Some(Cmd::Chats { .. }) | Some(Cmd::Contacts { .. }) | Some(Cmd::Print { .. }) | Some(Cmd::PrintLast { .. })
    );
    let mut lock = open_lock_file(&data_dir)?;
    let lock_guard = match lock.try_write() {
        Ok(guard) => Some(guard),
        Err(_) if read_only_is_fine => None,
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
            return outcome.map_err(|e| anyhow::anyhow!(e));
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

    let mut state = signal::SyncState { data_dir, own_aci: Some(signal::own_aci(&manager)) };

    match cmd {
        None => {
            let stream = signal::connect(&mut manager, &mut state).await?;
            eprintln!("Loading chats…");
            let threads = signal::list_threads(&manager, &state.data_dir, state.own_aci).await?;
            app::run(threads, state.own_aci, state.data_dir, manager, stream).await
        }

        Some(Cmd::Link) => {
            eprintln!("Linking successful. Device provisioned.");
            Ok(())
        }

        Some(Cmd::Chats { format }) => {
            if may_sync {
                signal::sync(&mut manager, &mut state).await?;
            }
            let threads = signal::list_threads(&manager, &state.data_dir, state.own_aci).await?;
            for entry in &threads {
                match format {
                    Format::Text => {
                        match entry.last_preview.as_deref() {
                            Some(p) if !p.is_empty() => println!("{}: {}", entry.name, p),
                            _ => println!("{}", entry.name),
                        }
                    }
                    Format::Json => {
                        let id = thread_id_string(&entry.thread);
                        println!("{}", serde_json::json!({
                            "id":        id,
                            "name":      entry.name,
                            "preview":   entry.last_preview,
                            "timestamp": iso_timestamp(entry.last_ts),
                        }));
                    }
                }
            }
            Ok(())
        }

        Some(Cmd::Contacts { format }) => {
            if may_sync {
                eprintln!("Syncing contacts…");
                signal::sync_contacts(&mut manager, &mut state).await?;
                if let Err(e) = signal::fetch_missing_profiles(&mut manager).await {
                    tracing::warn!("profile fetch: {e:#}");
                }
            }
            let (contacts, _) = signal::list_all_contacts(&manager, state.own_aci).await?;
            for entry in &contacts {
                match &entry.thread {
                    Thread::Contact(sid) => {
                        let uuid = sid.raw_uuid();
                        let name = entry.name.as_str();
                        match format {
                            Format::Text => println!("{} {}", uuid, name),
                            Format::Json => println!("{}", serde_json::json!({
                                "id": uuid.to_string(), "name": name, "type": "contact",
                            })),
                        }
                    }
                    Thread::Group(key) => {
                        let hex = hex_string(key);
                        match format {
                            Format::Text => println!("{} {}", hex, entry.name),
                            Format::Json => println!("{}", serde_json::json!({
                                "id": hex, "name": entry.name, "type": "group",
                            })),
                        }
                    }
                }
            }
            Ok(())
        }

        Some(Cmd::Print { format, recipient }) => {
            if may_sync {
                signal::sync(&mut manager, &mut state).await?;
            }
            let thread = signal::parse_thread_id(&recipient)?;
            let (messages, _) = signal::load_messages_and_reactions(&manager, &thread).await?;
            print_messages(&mut manager, &thread, &messages, &format).await;
            Ok(())
        }

        Some(Cmd::PrintLast { count, format, recipient }) => {
            if may_sync {
                signal::sync(&mut manager, &mut state).await?;
            }
            let thread = signal::parse_thread_id(&recipient)?;
            let (messages, _) = signal::load_messages_and_reactions(&manager, &thread).await?;
            let start = messages.len().saturating_sub(count);
            print_messages(&mut manager, &thread, &messages[start..], &format).await;
            Ok(())
        }

        Some(Cmd::Watch { format, recipient }) => {
            let thread = signal::parse_thread_id(&recipient)?;
            let (mut send_jobs, _send_socket) = ipc::serve(&state.data_dir)?;
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
                            job.finish(result.map(|_| ()));
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
    for aci in signal::mentioned_acis(content) {
        cached_sender_name(cache, manager, aci).await;
    }
}

/// Print messages with names resolved like the TUI does, limited to the
/// accounts that appear: the group roster (with cached profile fetches) first,
/// then the contact store for anyone else.
async fn print_messages<S: Store>(
    manager: &mut Manager<S, Registered>,
    thread: &Thread,
    messages: &[Content],
    format: &Format,
) {
    let wanted: std::collections::HashSet<Uuid> = messages
        .iter()
        .flat_map(|m| std::iter::once(m.metadata.sender.raw_uuid()).chain(signal::mentioned_acis(m)))
        .collect();
    let mut names = signal::load_sender_names(manager, thread, None, Some(&wanted)).await;
    for msg in messages {
        let uuid = msg.metadata.sender.raw_uuid();
        let sender_name = cached_sender_name(&mut names, manager, uuid).await;
        cache_mentioned_names(&mut names, manager, msg).await;
        let body = signal::message_body(msg, |u| signal::name_or_uuid(&names, u));
        println!("{}", format_one(format, msg.timestamp(), uuid, &sender_name, &body, None));
    }
}

fn iso_timestamp(ts_ms: u64) -> String {
    signal::ts_to_datetime(ts_ms).map_or_else(|| ts_ms.to_string(), |dt| dt.to_rfc3339())
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
        Format::Json => {
            let mut record = serde_json::json!({
                "timestamp":   iso_timestamp(ts_ms),
                "sender_uuid": sender_uuid.to_string(),
                "sender_name": sender_name,
                "body":        body,
            });
            if let Some(target) = edit_of {
                record["edit_of"] = serde_json::Value::String(iso_timestamp(target));
            }
            record.to_string()
        }
    }
}

fn thread_id_string(thread: &Thread) -> String {
    match thread {
        Thread::Contact(sid) => sid.raw_uuid().to_string(),
        Thread::Group(key)   => hex_string(key),
    }
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// ── Thread parsing ────────────────────────────────────────────────────────────

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
