# Stack

## Software

| Component | Choice | Version |
|-----------|--------|---------|
| Language | Rust, edition 2024 | stable toolchain |
| Signal client | presage | git revision `4b81740` (October 2026) |
| Signal transport | libsignal-service-rs, through presage | pinned by presage |
| Storage | SQLite via `presage-store-sqlite`, linked against the system `libsqlite3` | `default-features = false` |
| TUI | ratatui with the crossterm backend | ratatui 0.30, crossterm 0.29 |
| Async runtime | tokio, on a manually created 64 MB thread with a `LocalSet` | 1.48 |
| CLI | clap, derive API | 4 |
| Serialization | serde, serde_json | 1 |
| Instance lock | fd-lock | 4 |
| QR code | qr2term | 0.3 |
| Emoji shortcodes | emojis | 0.9 |
| Logging | tracing, tracing-subscriber | 0.1, 0.3 |
| Build | Cargo, `make` wrapper | |
| Tests | Rust built-in `cargo test` | |

One crate is patched: `libsqlite3-sys` comes from the Whisperfish fork of rusqlite, the same patch presage applies.

## Targets

| Target | Role |
|--------|------|
| OpenBSD amd64 | Primary target. Built manually with `make release`. |
| Linux x86_64 | CI target. GitHub Actions runs `cargo check`, `cargo clippy -D warnings` and `cargo test` on every push. |

## Build prerequisites

- `protoc` (libsignal-service-rs compiles the Signal protobuf definitions at build time).
- `pkg-config` and the SQLite development files. `.cargo/config.toml` sets `LIBSQLITE3_SYS_USE_PKG_CONFIG=1` so the system library is used instead of the bundled SQLCipher build; see `decisions.md`.
- A C compiler. The TLS stack (rustls) uses AWS-LC as its crypto provider and compiles it from source.

## Runtime

- Data directory `~/.local/share/sst` (from `directories::ProjectDirs`), overridable with `--db <path>`.
- Signal account: a linked device, provisioned with `sst link` by scanning a QR code on the phone.
- Pre-1.0.0: no migration code for sst's own files. If their format changes, `sst link` wipes and re-links.
