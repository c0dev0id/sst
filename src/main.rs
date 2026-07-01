mod app;
mod signal;
mod ui;

use anyhow::Context as _;
use clap::Parser;
use directories::ProjectDirs;
use futures::{StreamExt, channel::oneshot, future};
use presage::libsignal_service::configuration::SignalServers;
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

#[derive(Parser)]
#[clap(about = "Signal TUI client")]
struct Args {
    #[clap(long, help = "Re-link this device even if already registered")]
    relink: bool,

    #[clap(long, help = "Sync messages and print chat list, then exit")]
    list: bool,

    #[clap(long, help = "Sync contacts and print all contacts and groups, then exit")]
    contact_list: bool,

    #[clap(long, value_name = "UUID|HEX", help = "Send stdin as a message to a contact (UUID) or group (64-char hex)")]
    send: Option<String>,

    #[clap(long, value_name = "UUID|HEX", help = "Print full chat history as JSONL, then exit")]
    read: Option<String>,

    #[clap(long, value_name = "UUID|HEX", help = "Stream new incoming messages as JSONL (no history; runs until stream closes)")]
    read_stream: Option<String>,

    #[clap(long, help = "Path to the SQLite database (default: XDG data dir)")]
    db: Option<PathBuf>,
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
    let db_path = args.db.unwrap_or_else(|| {
        ProjectDirs::from("", "", "simple-signal-tui")
            .expect("could not determine data directory")
            .data_dir()
            .join("db")
    });
    let data_dir = db_path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&data_dir)?;

    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing::metadata::LevelFilter::WARN.into())
        .from_env_lossy()
        .add_directive("libsignal=error".parse().unwrap());

    if args.list {
        tracing_subscriber::fmt::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .init();
    } else {
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir.join("sst.log"))?;
        tracing_subscriber::fmt::fmt()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_env_filter(filter)
            .init();
    }

    let store = SqliteStore::open_with_passphrase(
        db_path.to_str().context("non-UTF-8 db path")?,
        None::<&str>,
        OnNewIdentity::Trust,
    )
    .await
    .context("failed to open store")?;

    let local = tokio::task::LocalSet::new();
    local.run_until(run(args.relink, args.list, args.contact_list, args.send, args.read, args.read_stream, store, data_dir)).await
}

async fn run<S: Store>(relink: bool, list: bool, contact_list: bool, send: Option<String>, read: Option<String>, read_stream: Option<String>, store: S, data_dir: std::path::PathBuf) -> anyhow::Result<()> {
    let mut manager = if relink {
        link_device(store).await?
    } else {
        Manager::load_registered(store).await.map_err(|_| {
            anyhow::anyhow!(
                "No existing registration found. Run with --relink to link this device."
            )
        })?
    };

    let mut state = signal::SyncState { data_dir, own_aci: None };

    if list {
        signal::sync(&mut manager, &mut state).await?;
        let threads = signal::list_threads(&manager, &state.data_dir, state.own_aci).await?;
        println!("--- {} chat(s) ---", threads.len());
        for entry in &threads {
            let preview = entry.last_preview.as_deref().unwrap_or("(no messages)");
            println!("{}: {}", entry.name, preview);
        }
        return Ok(());
    }

    if contact_list {
        signal::sync(&mut manager, &mut state).await?;
        let (contacts, _) = signal::list_all_contacts(&manager, state.own_aci).await?;
        for entry in &contacts {
            match &entry.thread {
                Thread::Contact(sid) => {
                    println!("{} {}", sid.raw_uuid(), entry.name);
                }
                Thread::Group(key) => {
                    let hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
                    println!("{} {}", hex, entry.name);
                }
            }
        }
        return Ok(());
    }

    if let Some(recipient) = send {
        let thread = parse_thread_id(&recipient)?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        let text = text.trim_end_matches(['\n', '\r']).to_string();
        if text.is_empty() {
            anyhow::bail!("nothing to send (stdin was empty)");
        }
        signal::send_to_thread(&mut manager, &thread, text).await?;
        return Ok(());
    }

    if let Some(recipient) = read {
        let thread = parse_thread_id(&recipient)?;
        let messages = signal::load_messages(&manager, &thread).await?;
        for msg in &messages {
            let sender_uuid = msg.metadata.sender.raw_uuid();
            let sender_name = signal::lookup_contact_name(&manager, sender_uuid).await;
            let body = signal::message_body(msg);
            println!("{}", json_line(msg.timestamp(), sender_uuid, &sender_name, &body));
        }
        return Ok(());
    }

    if let Some(recipient) = read_stream {
        let thread = parse_thread_id(&recipient)?;
        let mut stream = Box::pin(
            manager.receive_messages().await.context("failed to start receive stream")?,
        );
        // Drain the pending queue without emitting anything.
        while let Some(event) = stream.next().await {
            if matches!(event, Received::QueueEmpty) {
                break;
            }
        }
        // Emit new messages for this thread as they arrive.
        while let Some(event) = stream.next().await {
            let Received::Content(boxed) = event else { continue };
            if Thread::try_from(boxed.as_ref()).ok().as_ref() != Some(&thread) {
                continue;
            }
            let body = signal::message_body(&boxed);
            if body.is_empty() {
                continue;
            }
            let sender_uuid = boxed.metadata.sender.raw_uuid();
            let sender_name = signal::lookup_contact_name(&manager, sender_uuid).await;
            println!("{}", json_line(boxed.timestamp(), sender_uuid, &sender_name, &body));
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
        return Ok(());
    }

    let stream = signal::connect(&mut manager, &mut state).await?;
    let threads = signal::list_threads(&manager, &state.data_dir, state.own_aci).await?;

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::task::spawn_local(async move {
        futures::pin_mut!(stream);
        while let Some(event) = stream.next().await {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });

    app::run(threads, state.own_aci, state.data_dir, manager, rx).await
}

fn json_line(ts_ms: u64, sender_uuid: Uuid, sender_name: &str, body: &str) -> String {
    use chrono::{DateTime, Utc};
    let timestamp = DateTime::from_timestamp((ts_ms / 1000) as i64, 0)
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
        .unwrap_or_else(|| ts_ms.to_string());
    serde_json::json!({
        "timestamp": timestamp,
        "sender_uuid": sender_uuid.to_string(),
        "sender_name": sender_name,
        "body": body,
    })
    .to_string()
}

fn parse_thread_id(s: &str) -> anyhow::Result<Thread> {
    // UUID → 1:1 contact thread
    if let Ok(uuid) = s.parse::<Uuid>() {
        return Ok(Thread::Contact(ServiceId::Aci(uuid.into())));
    }
    // 64 hex chars → group master key
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16))
            .collect::<Result<_, _>>()?;
        let key: [u8; 32] = bytes.try_into().expect("32 bytes");
        return Ok(Thread::Group(key));
    }
    anyhow::bail!("invalid recipient '{}': expected a UUID (contact) or 64-char hex string (group)", s)
}

async fn link_device<S: Store>(store: S) -> anyhow::Result<Manager<S, Registered>> {
    let (tx, rx) = oneshot::channel();

    let (manager_result, _) = future::join(
        Manager::link_secondary_device(
            store,
            SignalServers::Production,
            "simple-signal-tui".to_string(),
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
