# Development notes

## Boundaries

- `src/`: task forms, list, conflict UI, Tauri commands. `styles.css` is the styling entry point. Task content is inserted as text, never HTML.
- `crates/core/`: platform-independent Rust models, validation, SQLite storage, Astraea HTTP client and tests. `cargo test` defaults to this crate so domain work does not require a GUI toolchain.
- `src-tauri/`: native shell, IPC commands, notification adapter, and a five-second reminder loop.
- `scripts/test-sync.mjs`: disposable integration environment using the actual sibling backend.

Keep task rules, sync and persistence in Rust. Keep the frontend focused on presentation. Native alarm scheduling should be behind a platform adapter rather than implemented with frontend timers.

SQLite currently stores a versioned task model and all local sync metadata in one atomic JSON snapshot row. This keeps the first implementation small and commits each change-feed page together with its cursor. It suits a personal prototype; a future migration can normalize records and outbox rows for larger datasets. Compare-and-swap writes reject stale app processes rather than silently overwriting newer data. Back up the database before schema changes. Task payloads have a schema version, but the local database and snapshot do not yet have a migration version. There is no export/import UI yet; both are prerequisites for changing persisted formats.

The database is `nothingtodo.sqlite3` in Tauri's per-user app data directory (`$XDG_DATA_HOME/dev.astraea.nothingtodo` on Linux, normally `~/.local/share/dev.astraea.nothingtodo`). It contains private task data in plaintext. Full database encryption at rest is an open product decision, not an implemented guarantee. Protected token storage is a separate requirement; if database encryption is adopted, its key recovery and backup behavior must be defined too. SQLite WAL files can contain recent edits too; close the app before copying the whole database directory.

## Sync rules

Each task UUID is one key in `v1/apps/nothingtodo/settings`; payloads include `schemaVersion: 1`, title, notes, completion and optional UTC reminder time. Device delivery receipts never leave local storage. Deletes retain local/server tombstones so another device cannot accidentally resurrect a task.

A new client replays `/changes?after=0`. The client applies each page and commits its cursor together before fetching the next. It pulls before pushing pending records with their original `baseVersion`. Both change-feed divergence and HTTP 409 preserve local edits for explicit resolution. Matching server values reconcile uncertain write acknowledgements only after replaying the entire feed, so an old matching event cannot erase a pending edit. Network failures retain pending changes for automatic retry. Unknown schemas stop replay without advancing the affected page's cursor.

Sync and local edits share one mutex in the native shell. This avoids acknowledging an older write over a new local edit, but long sync operations can delay editing and reminders. Releasing the lock around network I/O, with local revision checks on acknowledgement, is an early correctness fix; it must not wait for storage normalization. A single native worker owns all sync attempts. It coalesces edits within 750ms, polls every 30 seconds and retries failures with 5s–300s exponential backoff. Edits during an outage preserve the retry deadline; connect/manual/online/foreground signals request an immediate attempt. Startup sync requires reconnecting because credentials remain session-only. Sync status is held in Rust and emitted to the UI. There is no E2E encryption, signup, push delivery or server-scheduled alarm logic.

## Suggested development loop

Use `npm run tauri dev` on Linux for the fastest full app loop. Change UI styles in `src/styles.css`; Vite reloads them. Change domain behavior in `crates/core` and run its tests first. Before sharing a change, run the checks in the README and test the native flow that changed. Add regression tests for data loss, migrations, conflicts and scheduling rules.

Use a local backend account during development. Test with two app profiles/devices before changing sync behavior. Do not share local data between multiple running processes; a stale writer reports an error and needs a restart. CI checks the Linux build; Android needs its own device build once the SDK is configured.

## Revised milestones

These are planned changes, not capabilities of the current app. Foundation work comes first; protected credentials and the lock fix are prerequisites for the native reminder milestone. Styling can proceed independently in `src/styles.css`.

### 0. Foundation work before expanding the app

- **Finalize the app identifier before `tauri android init`.** The starter uses `dev.astraea.nothingtodo`; the final identifier has not been selected. It determines the Linux data directory and Android package identity. Changing it does not delete old data, but can leave it outside the new app's data directory and install a separate Android app. Provide an explicit migration for any existing users.
- **Prove the Android build on a real phone.** Configure the JDK, SDK and NDK, generate the Android project, and verify launch, SQLite persistence, HTTPS sync and notification permissions. Check the pinned `reqwest`/Rustls/AWS-LC and bundled SQLite dependencies with the NDK. Treat crypto-provider changes as responses to demonstrated build issues, not a prerequisite: [AWS-LC documents Android support](https://aws.github.io/aws-lc-rs/platform_support.html). Add an Android compile job once the project builds; keep device lifecycle tests distinct from CI compilation.
- **Introduce numbered local migrations and recovery.** Track the database version (for example with `PRAGMA user_version`), migrate existing unversioned snapshots transactionally, and refuse unsupported newer database versions without modifying them. Add versioned JSON export and validated import before changing the snapshot format or adding alarm state. Preserve pending edits, tombstones, conflicts and opaque records; exports must exclude credentials. Define whether imports restore the same profile or create new tasks, and test interrupted migrations and failed imports against existing data.
- **Ship forward-compatible sync before writing new record types or schema versions.** Decode the transport envelope separately from task payloads. Persist unsupported keys, fields and schema versions as opaque records, with their server versions and deletion state, in the same transaction as the page cursor. Show an upgrade indication, continue syncing supported records, and prevent older clients from editing or overwriting opaque data. Define how an unsupported replacement of a known task interacts with pending local edits and previously scheduled alarms. Test mixed-version pages, later upgrades, unknown tombstones and preservation across export/import. This requires a storage migration and is not just removing `deny_unknown_fields`.

### 1. Protected credentials and account identity

Use Linux Secret Service when available. On minimal Linux setups without a usable keyring, fall back to an explicit session-only mode; never persist plaintext tokens. On Android, encrypt the token with a Keystore-backed key and store the ciphertext privately. Handle locked/unavailable stores, key invalidation and disconnect/revocation without logging secrets.

Replace the origin-plus-token profile binding with an account identity strategy that permits token rotation and multiple device tokens while preventing cross-account uploads. The backend has no account identity endpoint. A reserved, typed marker record within the existing namespace is a candidate, not a settled design: it needs atomic initialization through `baseVersion`, concurrent-device tests, and the opaque-record handling from milestone 0. A new token must prove the expected remote identity before any pending local writes are uploaded. Backend endpoint changes require separately scoped work rather than invented client assumptions.

Acceptance: restart and resume can reconnect with protected credentials; session-only fallback remains usable; rotating credentials within the same account preserves local data; switching accounts cannot upload another account's pending records.

### 2. Fix sync locking and test worker behavior

Do not hold the store mutex across HTTP calls or the sync-client mutex for an entire attempt. Capture the request state, release locks, perform network I/O, then commit with local revision checks so acknowledgements cannot overwrite edits made during the request. Persist each applied page together with its cursor and keep sync attempts serialized. Credential replacement must invalidate or isolate in-flight responses from the previous session. This work precedes native reminders and does not depend on normalizing SQLite.

Keep manual requests and credential changes responsive, coalesce duplicate wake signals, and expose cancellation/progress for long first syncs. Bound work per pass so replaying hundreds of pages cannot starve local edits or alarm reconciliation.

Add deterministic tests for editing during a slow write, lost acknowledgements, session replacement, offline retries, repeated wake events and multi-page replay. Put reusable scheduling and reconciliation rules in core, but also test native worker orchestration and frontend busy/status behavior; core policy tests alone do not prove the shell remains responsive.

### 3. Native reminders and background lifecycle

First define a pure core function that computes the desired alarm set from local state. Platform adapters reconcile that set with OS schedules after edits, completion, deletion, sync, startup and recovery. Use stable alarm identifiers and idempotent reconciliation. Define cancellation behavior for unsupported records and conflicts before scheduling them.

Evaluate the installed notification plugin before writing Kotlin: its Android implementation already includes scheduling, persisted notification state and a boot restore receiver. Verify permission handling, cancellation, reboot/unlock recovery and missed-notification behavior on a device rather than assuming these meet our requirements.

Handle Android notification permission and exact-alarm access explicitly, with an inexact fallback and clear timing expectations. Exact-alarm eligibility depends on platform and distribution policy; consult the [Android alarm guidance](https://developer.android.com/develop/background-work/services/alarms) before selecting permissions. Native scheduled delivery and background synchronization are separate responsibilities.

Add Android background sync using protected credentials and an appropriate OS lifecycle adapter. Periodic WorkManager work has a [15-minute minimum and may be delayed](https://developer.android.com/reference/androidx/work/PeriodicWorkRequest); it cannot reproduce the current active-process 30-second polling interval. Completing a task elsewhere cancels the phone's alarm only after the phone learns that change. Offline devices can still fire stale alarms, so do not promise immediate cross-device cancellation.

Decide the product rules before implementing them:

- Whether all devices notify or users select a preferred device.
- A missed-reminder policy, including an overdue cutoff or summary on a new device's first sync, to avoid a notification storm from historical tasks.
- Whether snooze is local to a device or shared. The current action rewrites synced `remindAt`; a local snooze override is the proposed default and must be separate from future recurrence rules.
- Linux window-close behavior, tray/background operation and optional autostart. Add single-instance activation for a friendly second-launch experience while retaining database compare-and-swap protection.

Acceptance: test alarm reconciliation in core and verify permission denial, process death, reboot/unlock, sleep/wake, offline completion on another device, overdue first sync and timezone/DST changes on the target platforms. Current one-shot times remain absolute instants; recurrence needs its own model.

### 4. Normalize storage and improve conflict handling

After migrations and export/import are available, move from the whole JSON snapshot to per-record SQLite storage and a durable outbox. Preserve server versions, local revisions, opaque payloads, tombstones, cursor atomicity and delivery metadata. Test migration with unsynced edits and unresolved conflicts.

Retain the last-synced base value to support field-level three-way merging. Automatically merge independent changes only where their semantics are defined; keep manual resolution for competing edits, deletion-versus-edit and ambiguous reminder changes. This is a usability improvement after the safety prerequisites, not a substitute for them.

Measure first-sync duration, snapshot/outbox size and history growth. Local tombstone cleanup needs a safe strategy; do not prune blindly. Server history retention or snapshot/bootstrap APIs require separately planned backend work because the current contract retains all history.

### 5. Product features

Add recurring reminders, standalone alarms, lists/tags, search and keyboard shortcuts. Specify recurrence timezone/DST semantics, completion behavior and local snooze interaction first. Use the migration and forward-compatibility machinery before introducing schema 2 or new record types; test mixed app versions.

### 6. Styling and release

Replace the plain UI and starter icons, update template metadata such as `authors = ["you"]`, and complete signing and packaging. The identifier decision and first Android build belong to milestone 0, and Android compile CI should already exist by this point. Release checks include real-device lifecycle tests, migration/recovery tests and an explicit statement of current encryption and cross-device delivery guarantees.
