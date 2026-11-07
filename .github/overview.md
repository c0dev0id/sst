# Overview

sst is a terminal client for Signal. It is for people who work in a shell and want to read, send and script Signal messages without a desktop GUI. It runs as a linked device next to the user's phone.

## The problem

Signal Desktop is not available on OpenBSD, and `signal-cli` needs a Java runtime that does not run there either. A shell user needs a client that

- builds from source on OpenBSD and Linux,
- reads and writes messages with the keyboard only,
- can be driven from scripts and other programs, and
- keeps messages in a local store that can be read without a round-trip to Signal's servers.

## How sst solves it

- **One binary, two faces.** `sst` without arguments opens a vim-modal TUI. The subcommands `chats`, `contacts`, `print`, `print-last`, `recent`, `watch` and `send` give scripts the same data as text or NDJSON.
- **A linked device on presage.** sst links to the phone by QR code and speaks the Signal protocol through the presage library. Sessions, keys, contacts, groups and messages live in presage's SQLite store under `~/.local/share/sst`.
- **Signal semantics, not approximations.** Mentions are body ranges, replies carry quotes, edits and delete-for-everyone respect Signal's 24-hour window, and read receipts go to each author and sync to the user's other devices.
- **One session holder at a time.** A running TUI or `sst watch` holds the Signal session and serves sends for `sst send` over a local socket, so two processes never write ratchet state in parallel.
- **An interface for other tools.** The JSON output is a stable contract. The `mcpd` MCP host wraps it so an AI assistant can read chats and, when allowed, send messages.
