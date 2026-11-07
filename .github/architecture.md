# Architecture

## High-level architecture

Primary components are the seven source files and presage. Secondary components are the libraries presage builds on and the external parties sst talks to.

```mermaid
flowchart LR
    subgraph clients [Callers]
        term[Terminal user]
        scripts[Scripts and mcpd]
    end
    subgraph sst [sst process]
        main["main.rs<br/>CLI, instance lock, runtime"]
        app["app.rs<br/>TUI state and event loop"]
        ui["ui.rs<br/>rendering"]
        ipc["ipc.rs<br/>send socket"]
        signal["signal.rs<br/>Signal operations"]
        mentions["mentions.rs<br/>mention codec"]
        attachments["attachments.rs<br/>attachment pipeline"]
    end
    subgraph libs [Libraries]
        presage[presage Manager]
        lss[libsignal-service-rs]
        store[("SQLite store<br/>presage-store-sqlite")]
    end
    servers[(Signal servers)]
    phone[Primary device]

    term --> main
    scripts -->|subcommands, NDJSON| main
    scripts -->|sst send| ipc
    main --> app
    main --> signal
    app --> ui
    app --> signal
    app --> ipc
    ipc --> signal
    signal --> mentions
    signal --> attachments
    signal --> presage
    presage --> lss
    presage --> store
    lss <--> servers
    servers <--> phone
```

## Block diagram

Primary layers top to bottom. The secondary layer on the right holds the side files sst keeps next to presage's store.

```mermaid
flowchart TB
    subgraph presentation [Presentation]
        tui["TUI views: chat list, chat window, contact browser"]
        cli["CLI subcommands, text and NDJSON output"]
    end
    subgraph application [Application]
        loop["Event loop: key events, Signal stream, send jobs"]
        cmds["Commands: send, reply, edit, react, delete, upload, download"]
        sock["Send socket: server in the session holder, client in sst send"]
    end
    subgraph domain [Signal domain]
        msg["Message bodies, mentions, quotes, reactions, edits"]
        rcpt["Receipts and last-seen marks"]
        att["Attachment staging, upload, download"]
        disc["Startup drain, contact and group discovery"]
    end
    subgraph infra [Infrastructure]
        presage[presage Manager]
        sqlite[(presage SQLite store)]
        lock[Instance lock]
    end
    subgraph side [Side files]
        known[("known_contacts<br/>known_groups")]
        seen[("last_seen.json")]
    end
    presentation --> application --> domain --> infra
    disc --> known
    rcpt --> seen
```

## Data model

The SQLite store belongs to presage; sst reads it through the store API and never touches the schema. Shown are the tables sst depends on. The three side files are sst's own and are listed as entities for completeness; they are flat files, not tables.

```mermaid
erDiagram
    threads ||--o{ thread_messages : holds
    contacts |o--o| threads : "recipient_id = uuid"
    groups |o--o| threads : "group_master_key = master_key"
    profile_keys ||--o| profiles : describes
    contacts |o--o| profile_keys : "same uuid"

    threads {
        int id PK
        blob group_master_key UK
        text recipient_id UK
    }
    thread_messages {
        int ts PK
        int thread_id PK, FK
        text sender_service_id
        text destination_service_id
        blob content_body
    }
    contacts {
        blob uuid PK
        text name
        text phone_number
        blob profile_key
        int expire_timer
    }
    groups {
        blob master_key PK
        text title
        blob members
    }
    profile_keys {
        blob uuid PK
        blob key
    }
    profiles {
        blob uuid PK, FK
        text given_name
        text family_name
    }
    last_seen_json {
        text thread_id PK
        int newest_seen_ts
    }
    known_contacts {
        blob uuid
    }
    known_groups {
        blob master_key
    }
```

A row in `thread_messages` is a serialized `Content`, which is not always a message: receipts, remote deletes, call signalling and edit records share the table. `content_body` decides what a row is.
