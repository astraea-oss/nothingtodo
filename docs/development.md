# Development notes

## Boundaries

- `src/`: task forms, list, conflict UI, Tauri commands. `styles.css` is the styling entry point. Task content is inserted as text, never HTML.
- `crates/core/`: platform-independent Rust models, validation, SQLite storage, Astraea HTTP client and tests. `cargo test` defaults to this crate so domain work does not require a GUI toolchain.
- `src-tauri/`: native shell, IPC commands, notification adapter, and a five-second reminder loop.
- `scripts/test-sync.mjs`: disposable integration environment using the actual sibling backend.

Keep task rules, sync and persistence in Rust. Keep the frontend focused on presentation. Native alarm scheduling should be behind a platform adapter rather than implemented with frontend timers.

SQLite currently stores a versioned task model and all local sync metadata in one atomic JSON snapshot row. This keeps the first implementation small and commits each change-feed page together with its cursor. It suits a personal prototype; a future migration can normalize records and outbox rows for larger datasets. Compare-and-swap writes reject stale app processes rather than silently overwriting newer data. Back up the database before schema changes. There is no export/migration UI yet.

The database is `nothingtodo.sqlite3` in Tauri's per-user app data directory (`$XDG_DATA_HOME/dev.astraea.nothingtodo` on Linux, normally `~/.local/share/dev.astraea.nothingtodo`). It contains private task data in plaintext. SQLite WAL files can contain recent edits too; close the app before copying the whole database directory.

## Sync rules

Each task UUID is one key in `v1/apps/nothingtodo/settings`; payloads include `schemaVersion: 1`, title, notes, completion and optional UTC reminder time. Device delivery receipts never leave local storage. Deletes retain local/server tombstones so another device cannot accidentally resurrect a task.

A new client replays `/changes?after=0`. The client applies each page and commits its cursor together before fetching the next. It pulls before pushing pending records with their original `baseVersion`. Both change-feed divergence and HTTP 409 preserve local edits for explicit resolution. Matching server values reconcile uncertain write acknowledgements only after replaying the entire feed, so an old matching event cannot erase a pending edit. Network failures retain pending changes for automatic retry. Unknown schemas stop replay without advancing the affected page's cursor.

Sync and local edits share one mutex in the native shell. This avoids acknowledging an older write over a new local edit, but long sync operations can delay editing and reminders. A future outbox worker should release the lock around network I/O and use local revision checks on acknowledgement. A single native worker owns all sync attempts. It coalesces edits within 750ms, polls every 30 seconds and retries failures with 5s–300s exponential backoff. Edits during an outage preserve the retry deadline; connect/manual/online/foreground signals request an immediate attempt. Startup sync requires reconnecting because credentials remain session-only. Sync status is held in Rust and emitted to the UI. There is no E2E encryption, signup, push delivery or server-scheduled alarm logic.

## Suggested development loop

Use `npm run tauri dev` on Linux for the fastest full app loop. Change UI styles in `src/styles.css`; Vite reloads them. Change domain behavior in `crates/core` and run its tests first. Before sharing a change, run the checks in the README and test the native flow that changed. Add regression tests for data loss, migrations, conflicts and scheduling rules.

Use a local backend account during development. Test with two app profiles/devices before changing sync behavior. Do not share local data between multiple running processes; a stale writer reports an error and needs a restart. CI checks the Linux build; Android needs its own device build once the SDK is configured.

## Next milestones

1. **Native reminders:** Android OS scheduling, cancellation/rescheduling on edits, completion and sync, reboot recovery, permission handling, and exact-alarm policy where required. Linux needs tray/background lifecycle and optional autostart. Test sleep/wake, missed alarms, app termination and timezone/DST changes. Current one-shot reminders are absolute instants; recurring timezone-aware alarms need a separate model.
2. **Credentials and profiles:** Linux Secret Service, Android Keystore, explicit account binding, token rotation and profile switching without leaking one account's records into another.
3. **Sync scalability:** move from snapshot serialization to a per-record SQLite outbox, release storage locks during network requests with revision checks on acknowledgements, and add OS lifecycle adapters for Android background operation. Automatic sync currently runs while the process is active. Continue to use the existing backend protocol.
4. **Product features:** recurring reminders, standalone alarms without todo semantics, lists/tags, search, and keyboard shortcuts. Define recurrence semantics and schema migrations before adding them.
5. **Styling and release:** replace the plain UI, choose branding/icons and final identifier, add Android device CI, then signing and packaging. Generated icons are starter Tauri placeholders.
