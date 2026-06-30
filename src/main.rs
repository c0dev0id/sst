mod app;
mod signal;
mod ui;

use anyhow::Context as _;
use clap::Parser;
use directories::ProjectDirs;
use futures::{channel::oneshot, future};
use presage::libsignal_service::configuration::SignalServers;
use presage::manager::Registered;
use presage::model::identity::OnNewIdentity;
use presage::store::Store;
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
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing::metadata::LevelFilter::WARN.into())
        .from_env_lossy()
        .add_directive("libsignal=error".parse().unwrap());
    tracing_subscriber::fmt::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();

    let db_path = args.db.unwrap_or_else(|| {
        ProjectDirs::from("", "", "simple-signal-tui")
            .expect("could not determine data directory")
            .data_dir()
            .join("db")
    });

    std::fs::create_dir_all(db_path.parent().unwrap())?;

    let store = SqliteStore::open_with_passphrase(
        db_path.to_str().context("non-UTF-8 db path")?,
        None::<&str>,
        OnNewIdentity::Trust,
    )
    .await
    .context("failed to open store")?;

    let local = tokio::task::LocalSet::new();
    local.run_until(run(args.relink, args.list, store, db_path.parent().unwrap().to_path_buf())).await
}

async fn run<S: Store>(relink: bool, list: bool, store: S, data_dir: std::path::PathBuf) -> anyhow::Result<()> {
    let mut manager = if relink {
        link_device(store).await?
    } else {
        Manager::load_registered(store).await.map_err(|_| {
            anyhow::anyhow!(
                "No existing registration found. Run with --relink to link this device."
            )
        })?
    };

    let state = signal::SyncState { data_dir };
    signal::sync(&mut manager, &state).await?;
    let threads = signal::list_threads(&manager, &state).await?;

    if list {
        println!("--- {} chat(s) ---", threads.len());
        for entry in &threads {
            let preview = entry.last_preview.as_deref().unwrap_or("(no messages)");
            println!("{}: {}", entry.name, preview);
        }
        return Ok(());
    }

    app::run(threads).await
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
