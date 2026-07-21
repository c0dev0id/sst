mod app;
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
use presage::libsignal_service::protocol::ServiceId;
use presage::model::messages::Received;
use presage::store::{ContentExt, Store, Thread};
use presage::Manager;
use presage_store_sqlite::SqliteStore;
use std::path::PathBuf;
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

    // send/print/print-last are one-shot operations (pure reads or a single
    // atomic DB write) that do not hold a persistent Signal WebSocket.  They
    // are safe to run concurrently with the TUI or `sst watch`, so they skip
    // the exclusive lock.  All other subcommands maintain persistent
    // connections or sync session state and must hold the exclusive lock to
    // prevent ratchet divergence.
    let needs_lock = !matches!(
        args.cmd,
        Some(Cmd::Send { .. }) | Some(Cmd::Print { .. }) | Some(Cmd::PrintLast { .. })
    );

    if needs_lock {
        let lock_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(data_dir.join("sst.lock"))
            .context("failed to open lock file")?;
        let mut lock_holder = fd_lock::RwLock::new(lock_file);
        // _lock_guard stays alive for the entire body below (until this block ends).
        let _lock_guard = lock_holder.try_write().map_err(|_| {
            anyhow::anyhow!(
                "another sst instance is already running\n\
                 (only one instance may use the Signal session at a time)"
            )
        })?;
        run_inner(args.cmd, db_path, data_dir).await
    } else {
        run_inner(args.cmd, db_path, data_dir).await
    }
}

async fn run_inner(cmd: Option<Cmd>, db_path: PathBuf, data_dir: PathBuf) -> anyhow::Result<()> {
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

    let store = SqliteStore::open_with_passphrase(
        db_path.to_str().context("non-UTF-8 db path")?,
        None::<&str>,
        OnNewIdentity::Trust,
    )
    .await
    .context("failed to open store")?;

    let local = tokio::task::LocalSet::new();
    local.run_until(run(cmd, store, data_dir)).await
}

async fn run<S: Store>(
    cmd: Option<Cmd>,
    store: S,
    data_dir: PathBuf,
) -> anyhow::Result<()> {
    let mut manager = if matches!(cmd, Some(Cmd::Link)) {
        link_device(store).await?
    } else {
        Manager::load_registered(store).await.map_err(|_| {
            anyhow::anyhow!(
                "No existing registration found. Run `sst link` to link this device."
            )
        })?
    };

    let mut state = signal::SyncState { data_dir, own_aci: None };

    match cmd {
        None => {
            let stream = signal::connect(&mut manager, &mut state).await?;
            let threads = signal::list_threads(&manager, &state.data_dir, state.own_aci).await?;
            app::run(threads, state.own_aci, state.data_dir, manager, stream).await
        }

        Some(Cmd::Link) => Ok(()),

        Some(Cmd::Chats { format }) => {
            signal::sync(&mut manager, &mut state).await?;
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
                            "id":      id,
                            "name":    entry.name,
                            "preview": entry.last_preview,
                            "last_ts": entry.last_ts,
                        }));
                    }
                }
            }
            Ok(())
        }

        Some(Cmd::Contacts { format }) => {
            eprintln!("Syncing contacts…");
            signal::sync_contacts(&mut manager, &mut state).await?;
            let resolved = signal::fetch_missing_profiles(&mut manager).await.unwrap_or_default();
            let (contacts, _) = signal::list_all_contacts(&manager, state.own_aci).await?;
            for entry in &contacts {
                match &entry.thread {
                    Thread::Contact(sid) => {
                        let uuid = sid.raw_uuid();
                        let name = resolved.get(&uuid).map(String::as_str).unwrap_or(&entry.name);
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
            signal::sync(&mut manager, &mut state).await?;
            let thread = parse_thread_id(&recipient)?;
            let messages = signal::load_messages(&manager, &thread).await?;
            print_messages(&manager, &messages, &format).await;
            Ok(())
        }

        Some(Cmd::PrintLast { count, format, recipient }) => {
            signal::sync(&mut manager, &mut state).await?;
            let thread = parse_thread_id(&recipient)?;
            let messages = signal::load_messages(&manager, &thread).await?;
            let start = messages.len().saturating_sub(count);
            print_messages(&manager, &messages[start..], &format).await;
            Ok(())
        }

        Some(Cmd::Watch { format, recipient }) => {
            let thread = parse_thread_id(&recipient)?;
            // BTreeSet allows evicting entries older than the dedup window in O(log n).
            let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
            let start_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            'reconnect: loop {
                let mut stream = Box::pin(match manager.receive_messages().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("receive_messages failed: {e}, retrying in 5s");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue 'reconnect;
                    }
                });

                while let Some(event) = stream.next().await {
                    let Received::Content(boxed) = event else { continue };
                    let ts = boxed.timestamp();
                    if ts < start_ts { continue; }
                    if Thread::try_from(boxed.as_ref()).ok().as_ref() != Some(&thread) { continue; }
                    let body = signal::message_body(&boxed);
                    if body.is_empty() { continue; }
                    // Evict timestamps older than 60 s to keep the dedup set bounded.
                    let horizon = ts.saturating_sub(60_000);
                    while seen.first().is_some_and(|&t| t < horizon) {
                        seen.pop_first();
                    }
                    if !seen.insert(ts) { continue; }
                    let sender_uuid = boxed.metadata.sender.raw_uuid();
                    let sender_name = signal::lookup_contact_name(&manager, sender_uuid).await;
                    let line = format_one(&format, ts, sender_uuid, &sender_name, &body);
                    println!("{}", line);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }

                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }

        Some(Cmd::Send { recipient, text, attach }) => {
            let thread = parse_thread_id(&recipient)?;
            let text = match text {
                Some(t) => t,
                None => {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf.trim_end_matches(['\n', '\r']).to_string()
                }
            };
            if text.is_empty() && attach.is_empty() {
                anyhow::bail!("nothing to send (no text and no attachments)");
            }
            let mut staged = Vec::new();
            for path in &attach {
                staged.push(
                    signal::stage_attachment(path)
                        .map_err(|e| anyhow::anyhow!("{}", e))?
                );
            }
            let pointers = if staged.is_empty() {
                Vec::new()
            } else {
                signal::upload_staged_attachments(&mut manager, &staged).await
                    .map_err(|(msg, _)| anyhow::anyhow!("{}", msg))?
            };
            signal::send_to_thread(&mut manager, &thread, text, pointers).await?;
            Ok(())
        }
    }
}

// ── Output helpers ────────────────────────────────────────────────────────────

async fn print_messages<S: Store>(
    manager: &Manager<S, Registered>,
    messages: &[Content],
    format: &Format,
) {
    let mut names: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();
    for msg in messages {
        let uuid = msg.metadata.sender.raw_uuid();
        if !names.contains_key(&uuid) {
            let name = signal::lookup_contact_name(manager, uuid).await;
            names.insert(uuid, name);
        }
        let sender_name = names.get(&uuid).map(String::as_str).unwrap_or("");
        let body = signal::message_body(msg);
        println!("{}", format_one(format, msg.timestamp(), uuid, sender_name, &body));
    }
}

fn format_one(format: &Format, ts_ms: u64, sender_uuid: Uuid, sender_name: &str, body: &str) -> String {
    match format {
        Format::Text => {
            format!("[{}] {}: {}", signal::fmt_ts_long(ts_ms), sender_name, body)
        }
        Format::Json => {
            use chrono::{DateTime, Utc};
            let ts = DateTime::from_timestamp((ts_ms / 1000) as i64, 0)
                .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
                .unwrap_or_else(|| ts_ms.to_string());
            serde_json::json!({
                "timestamp":   ts,
                "sender_uuid": sender_uuid.to_string(),
                "sender_name": sender_name,
                "body":        body,
            }).to_string()
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

fn parse_thread_id(s: &str) -> anyhow::Result<Thread> {
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
    anyhow::bail!(
        "invalid recipient '{}': expected a UUID (contact) or 64-char hex string (group)",
        s
    )
}

// ── Device linking ────────────────────────────────────────────────────────────

async fn link_device<S: Store>(store: S) -> anyhow::Result<Manager<S, Registered>> {
    let (tx, rx) = oneshot::channel();

    let (manager_result, _) = future::join(
        Manager::link_secondary_device(
            store,
            SignalServers::Production,
            "sst".to_string(),
            tx,
        ),
        async move {
            match rx.await {
                Ok(url) => {
                    eprintln!("Scan this QR code with your Signal app:");
                    qr2term::print_qr(url.to_string()).unwrap_or_else(|e| {
                        eprintln!("QR render failed: {e}");
                        eprintln!("URL: {url}");
                    });
                }
                Err(e) => error!("provisioning cancelled: {e}"),
            }
        },
    )
    .await;

    manager_result.context("device linking failed")
}
