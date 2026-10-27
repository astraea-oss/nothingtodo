# Development notes

## Boundaries

- `src/`: task forms, list, conflict UI, Tauri commands. `styles.css` is the styling entry point. Task content is inserted as text, never HTML.
- `crates/core/`: platform-independent Rust models, validation, SQLite storage, Astraea HTTP client and tests. `cargo test` defaults to this crate so domain work does not require a GUI toolchain.
- `src-tauri/`: native shell, IPC commands, notification adapter, and a five-second reminder loop.
- `scripts/test-sync.mjs`: disposable integration environment using the actual sibling backend.

Keep task rules, sync and persistence in Rust. Keep the frontend focused on presentation. Native alarm scheduling should be behind a platform adapter rather than implemented with frontend timers.

SQLite currently stores a versioned task model and all local sync metadata in one atomic JSON snapshot row. This keeps the first implementation small and commits each change-feed page together with its cursor. It suits a personal prototype; a future migration can normalize records and outbox rows for larger datasets. Compare-and-swap writes reject stale app processes rather than silently overwriting newer data. Back up the database before schema changes. Task payloads have a schema version, but the local database and snapshot do not yet have a migration version. There is no export/import UI yet. Numbered migrations and a recoverable backup are prerequisites for persisted format changes; forward-compatible storage precedes the new versioned export/import format.

The database is `nothingtodo.sqlite3` in Tauri's per-user app data directory (`$XDG_DATA_HOME/dev.astraea.nothingtodo` on Linux, normally `~/.local/share/dev.astraea.nothingtodo`). It contains private task data in plaintext. Full database encryption at rest is an open product decision, assigned to milestone 2 before credential implementation is finalized; it is not an implemented guarantee. Protected token storage is a separate requirement; if database encryption is adopted, its key recovery and backup behavior must be defined too. SQLite WAL files can contain recent edits too; close the app before copying the whole database directory.

## Sync rules

Each task UUID is one key in `v1/apps/nothingtodo/settings`; payloads include `schemaVersion: 1`, title, notes, completion and optional UTC reminder time. Device delivery receipts never leave local storage. Deletes retain local/server tombstones so another device cannot accidentally resurrect a task.

A new client replays `/changes?after=0`. The client applies each page and commits its cursor together before fetching the next. It pulls before pushing pending records with their original `baseVersion`. Both change-feed divergence and HTTP 409 preserve local edits for explicit resolution. Matching server values reconcile uncertain write acknowledgements only after replaying the entire feed, so an old matching event cannot erase a pending edit. Network failures retain pending changes for automatic retry. Unknown schemas stop replay without advancing the affected page's cursor.

Sync and local edits share one mutex in the native shell. This avoids acknowledging an older write over a new local edit, but long sync operations can delay editing and reminders. Releasing the lock around network I/O, with local revision checks on acknowledgement, is an early correctness fix; it must not wait for storage normalization. A single native worker owns all sync attempts. It coalesces edits within 750ms, polls every 30 seconds and retries failures with 5s–300s exponential backoff. Edits during an outage preserve the retry deadline; connect/manual/online/foreground signals request an immediate attempt. Startup sync requires reconnecting because credentials remain session-only. Sync status is held in Rust and emitted to the UI. There is no E2E encryption, signup, push delivery or server-scheduled alarm logic.

## Suggested development loop

Use `npm run tauri dev` on Linux for the fastest full app loop. Change UI styles in `src/styles.css`; Vite reloads them. Change domain behavior in `crates/core` and run its tests first. Before sharing a change, run the checks in the README and test the native flow that changed. Add regression tests for data loss, migrations, conflicts and scheduling rules.

Use a local backend account during development. Test with two app profiles/devices before changing sync behavior. Do not share local data between multiple running processes; a stale writer reports an error and needs a restart. CI checks the Linux build; Android needs its own device build once the SDK is configured.

## Roadmap

Everything below is planned unless explicitly described as current behavior. Deliver a small first release with safe persistence/recovery, responsive sync, protected credentials and device-tested one-shot reminders. Storage normalization, automatic merging, recurrence and standalone alarms are later work. Styling can proceed independently in `src/styles.css`.

The lock fix is an early correctness task: it depends on migration support for persisted local revisions, but not on the account identity design or storage normalization. Native local alarms and Android background sync have separate acceptance checkpoints; a working alarm adapter must not be presented as proof of reliable cross-device cancellation.

### 0a. App identity, distribution and Android feasibility

- **Finalize the identifier before `tauri android init`.** The starter uses `dev.astraea.nothingtodo`; the final identifier has not been selected. It determines the Linux data directory and Android package identity. Changing it does not delete old data, but can leave it outside the new app's data directory and install a separate Android app. Define migration for any existing profiles.
- **Choose initial distribution targets.** Decide whether Android initially ships through Play, F-Droid or sideloading, and choose the first Linux format from Flatpak, AppImage or deb/rpm. Validate dependency/license compatibility and channel policies before committing to packaging. Distribution affects exact-alarm policy, credential access, notifications, tray integration and autostart; sideloading does not remove Android's OS permission or power-management restrictions. Keep later channels optional.
- **Prove the Android build on a real phone.** Configure JDK/SDK/NDK, generate the project and verify launch, SQLite persistence, HTTPS sync and notification permissions. Check the pinned `reqwest`/Rustls/AWS-LC and bundled SQLite dependencies with the NDK. Change crypto providers only in response to demonstrated issues: [AWS-LC documents Android support](https://aws.github.io/aws-lc-rs/platform_support.html). Add Android compile CI after the project builds; real-device lifecycle tests remain separate.
- **Measure existing behavior early.** Record first-sync duration and page count, database/snapshot size, lock wait time and edit/reminder responsiveness with representative small and large histories, including weak connectivity. Set performance budgets from those measurements. Recheck them after the lock fix and before deciding on normalization.

Acceptance: record the identifier and initial channels, document their credential/lifecycle constraints, run the app and sync on a phone, add a passing Android compile job, and capture a reproducible performance baseline. Do not require every prospective distribution format before continuing.

### 0b. Migrations, compatibility and recovery

Implement numbered migrations first, forward-compatible record handling next, then export/import against that representation. Version the export format from its first release. Before the first migration, retain the existing closed-app directory backup procedure so the original database remains recoverable.

**Numbered migrations:** track the database version (for example with `PRAGMA user_version`), migrate unversioned snapshots transactionally and refuse newer unsupported database versions without modifying them. Add persisted per-record local revisions before implementing the lock fix. Preserve pending changes, tombstones, conflicts and delivery state, and test failures/interruption without acknowledging uncommitted data.

**Compatibility semantics:** decode transport envelopes separately from task payloads. Preserve genuinely additive fields in supported records when saving, rather than discarding them or making every extended record read-only. Define compatibility by meaning, not just JSON shape: an optional display field can be additive, while recurrence can alter completion and scheduling semantics even when represented by optional fields. Breaking changes or unsupported behavioral capabilities must be read-only to older clients, using explicit schema/capability rules. Do not assume recurrence belongs in schema 1 merely because its field is optional.

Persist unknown record types and unsupported payloads as opaque data, with server versions and tombstone state, atomically with the page cursor. Continue syncing supported records and show an upgrade indication. Older clients must never overwrite opaque records or drop unknown fields. Specify what happens to pending edits and scheduled alarms when a previously known task becomes unsupported. Test mixed-version pages, unknown tombstones and later upgrades.

**Rollout policy:** ship a compatibility release and establish a minimum supported client version before any supported device writes account markers, new record types or incompatible task data. Today's client rejects non-UUID keys and non-task payloads; a UUID-shaped marker does not avoid payload rejection. Deleting a marker cannot repair old clients because its creation remains in history. Test an actual previous client against an upgraded account and document its required upgrade and recovery behavior. Do not claim transparent compatibility for already-installed code that lacks this machinery.

**Recovery semantics:** export/import must preserve opaque data and exclude credentials. Keep two distinct user operations:

- **Restore the same profile:** validate account identity before uploads; restore the snapshot and its cursor only as a coherent pair. Reconcile with remote changes and tombstones before retrying pending edits, so an old backup cannot silently resurrect a remote deletion. Never advance to a cursor whose page was not applied. Define when to replay from zero instead. Treat a restored account binding as a claim to verify, not authorization to upload.
- **Copy tasks into another profile:** create fresh task IDs, preserve supported task content and additive fields, and start with no inherited server versions, cursor, account binding or pending operations from the source. Do not upload unsupported opaque records into another account; retain them in the original backup and report exclusions.

Device-local receipts, snoozes and notification-ID mappings are not portable by default. Define whether a same-device restore retains or resets them, clear/reconcile stale OS schedules, and make second-device imports apply the missed-reminder policy rather than suppress notifications based on another device's receipts. State these choices in the UI before import.

Acceptance: test migration failure, mixed-version replay and preservation of unknown fields/data; round-trip a versioned backup; restore an old backup after remote deletions; restore pending edits after token rotation; and import onto a second device. Verify cursor/binding handling, receipts, snoozes, alarm mappings and remote writes in each scenario. Failed or unverified imports must preserve existing data and block uploads. Exported copies remain private even when credentials are excluded.

### 1. Responsive sync and worker correctness

Start after the migration/local-revision prerequisite in 0b; do not wait for protected credentials or account marker design. Never hold the store mutex across HTTP calls or the sync-client mutex for an entire attempt. Capture request state, release locks, perform I/O, then commit with revision checks so acknowledgements cannot overwrite concurrent edits. Keep each applied page and its cursor atomic and keep network attempts serialized. Credential replacement must isolate or invalidate responses from the previous session.

Keep manual requests and connection changes responsive, coalesce wake signals, and expose cancellation/progress for long initial downloads. Bound replay work per pass so large histories cannot starve edits or reminder checks. This fix does not depend on normalized SQLite.

Acceptance: deterministic tests cover edits during slow writes, lost acknowledgements, session replacement, offline retries, wake storms and multi-page replay. Test the native worker and frontend busy/status behavior as well as core policy. Compare responsiveness with the 0a baseline; queued network work must not hold local editing or reminder checks behind a request timeout.

### 2. Protected credentials, account migration and encryption decision

Select credential adapters for the initial packaging targets from 0a. Use Linux Secret Service where available, with an explicit session-only fallback on setups without a usable keyring; never persist plaintext tokens. For Flatpak, evaluate the [Secret portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Secret.html): it supplies an app-specific secret for encrypting sandbox data, not a transparent Secret Service API. On Android, encrypt tokens with a Keystore-backed key and store ciphertext privately. Handle unavailable/locked stores, key invalidation, revocation and disconnect without logging secrets.

**Decide encryption at rest by this milestone.** Record whether database encryption is required for the first release or deferred, its threat model, key storage/recovery and backup/export rules. If selected, coordinate key management with credentials and include encryption in the first required storage migration rather than scheduling a second rewrite by default. SQLCipher is a candidate to evaluate, not a selected dependency. Required protection must not wait for optional normalization. If deferred, state clearly that task data and exports remain plaintext.

**Account identity and migration:** replace the current origin-plus-token fingerprint with a strategy supporting rotation and multiple device tokens without cross-account uploads. The backend has no identity endpoint. A reserved typed marker in the existing namespace remains a candidate, subject to the compatibility rollout in 0b, atomic `baseVersion` initialization and concurrent-device tests. It is not yet a verified design or a reason to invent client endpoints.

For an existing profile, authenticate with the original token and verify its existing origin/token fingerprint before creating or adopting the marker; persist the new identity binding transactionally while preserving pending edits. A replacement token must demonstrate the already-established expected remote identity before any pending upload. Creating a marker in whichever account accepts a replacement token does not establish continuity with the original account.

If the original token is unavailable/revoked and no trustworthy remote identity was established, preserve the local profile, block uploads and offer backup/export or an explicit copy into a separately configured profile. Do not silently rebind. Specify missing, deleted and changed marker behavior: quarantine the connection and require explicit recovery rather than automatically replacing the expected identity. A fresh profile can initialize an identity only through a separately defined onboarding flow.

Acceptance: protected credentials reconnect after restart/resume; session-only fallback works; original-token migration preserves pending edits; same-account token rotation works; wrong-account tokens, revoked originals and missing/deleted/changed markers cannot trigger unsafe uploads. Record and test the encryption decision, key-loss recovery and backup rules for the chosen packaging targets.

### 3a. Native one-shot reminders and notification actions

Define a pure core function for the desired alarm set. Platform adapters reconcile it after edits, completion, deletion, sync, startup and recovery. Define cancellation for unsupported records and unresolved conflicts. Store a collision-safe task UUID to 32-bit notification-ID mapping; do not truncate/hash UUIDs without collision handling. Support mapping recovery and cancellation when restoring a profile.

Evaluate the installed notification plugin before writing replacements. In Android plugin 2.7.0, the one-shot `allowWhileIdle` path selects a wakeup alarm, whereas the default path uses non-wakeup `RTC`. Set and test `allowWhileIdle` for reminders that must wake a sleeping phone. Repeating schedule paths differ; do not extrapolate the one-shot behavior to future recurrence.

The plugin manifest does not declare an exact-alarm permission. If exact delivery is required and permitted, the app must declare the appropriate permission and handle access being denied or revoked. Without exact-alarm access the plugin takes an inexact path on relevant Android versions, subject to platform exemptions. Handle notification permission separately and describe the inexact fallback. Follow [Android alarm guidance](https://developer.android.com/develop/background-work/services/alarms) and the selected channel's policy; declaring a permission alone does not ensure access.

The plugin persists its own schedule and restores it at boot. SQLite remains the source of truth, but the restored copy can be stale until native background work or the app reconciles it. Test reboot/unlock behavior and document that recovery does not require a webview in principle but does require a working native execution path. Do not claim SQLite automatically governs the plugin's boot receiver.

Decide and document the product rules before implementing adapters:

- Whether every device notifies or a preferred device is selected.
- A missed-reminder cutoff or summary, particularly on first sync, so historical tasks cannot cause a notification storm.
- One-shot timezone meaning: retain the current fixed-instant behavior for the first release, displaying it in the device's current timezone. Traveling changes the displayed local time, not the stored instant. Floating local-time reminders require an explicit future model.
- Device-local snooze is the proposed default. Store it separately from synced `remindAt` and future recurrence, with explicit restore/reset behavior. This also avoids a server write, retained history event and conflict opportunity per snooze. Do not claim it is implemented yet.
- Complete and Snooze actions on notifications. Prove cold-process handling, durable local mutation, rescheduling/cancellation and queued sync without relying on an open webview. Complete is shared task state; local Snooze stays device-local. Make repeated or outdated action delivery safe.
- Linux close-window behavior, tray/background operation, optional autostart and single-instance activation. Validate these in the selected package format and desktop environment while retaining database compare-and-swap protection.

Acceptance: test reconciliation in core and permission denial/revocation, sleep/wake, process death, notification actions with the process stopped, reboot/unlock, missed first-sync alarms, mapping collisions, backup recovery and timezone/DST changes on devices. Local one-shot alarm acceptance is distinct from background cross-device sync acceptance.

### 3b. Android background execution checkpoint

Before committing to an adapter, prove a native background job can open core storage without a webview, obtain protected credentials, perform sync, and reconcile OS schedules. Coordinate that job with the foreground worker and credential/session changes. Current compare-and-swap detects stale writers; it does not coordinate two Store instances or processes. Select a single-owner or transaction/reload strategy, and test its process/lifecycle assumptions explicitly.

Periodic WorkManager work has a [15-minute minimum and can be delayed](https://developer.android.com/reference/androidx/work/PeriodicWorkRequest); it cannot provide the active-process 30-second interval. Completion elsewhere cancels an alarm only after the phone learns the change. Offline devices can still fire stale alarms, even with protected credentials and background sync. Decide whether best-effort background sync ships in the first release based on this checkpoint, and disclose the supported behavior; local alarm delivery must not depend on connectivity.

Acceptance: demonstrate foreground/background overlap, concurrent edits, credential replacement, key unavailability, worker/process restart and multi-page replay without data loss, stale uploads or a locked-out foreground app. Reconcile schedules after successful background changes. Record actual device constraints and failure behavior rather than assuming a Rust timer survives Android suspension.

### First release boundary

Ship after the relevant checkpoints above demonstrate safe persistence/recovery, compatible sync and rollout rules, responsive editing, protected credentials (with an explicit session-only fallback), and device-tested one-shot reminders with Complete/Snooze actions. Document missed reminders, fixed-instant timezone behavior, encryption status and stale cross-device alarms. Finish initial-channel packaging, branding/icons, template metadata such as `authors = ["you"]`, signing and release validation here; styling need not wait for the later work below.

Acceptance: run migration/recovery, sync concurrency and real-device lifecycle checks on supported versions and the selected Linux/Android channels. Publish the minimum supported client version and exact background-sync guarantees. Native background sync is included only if 3b passes; otherwise clearly state that cross-device changes are learned while active. Recurrence, standalone alarms, field-level merging and normalization are not release gates unless measurements or safety requirements make a specific storage change necessary.

### 4. Conditional storage normalization and conflict improvements

Use the baseline from 0a and post-lock-fix measurements to decide whether normalized per-record SQLite storage and an outbox are needed. Document a trigger such as an exceeded performance budget or required transaction/concurrency capability. Normalization must preserve server/local revisions, opaque fields/payloads, tombstones, cursor atomicity, delivery metadata and any selected encryption. Avoid duplicate storage migrations where practical without postponing required security fixes.

For field-level three-way merging, retain the last-synced base value and define semantics before merging automatically. Independent edits can merge; deletion-versus-edit, competing reminder changes and unsupported behavioral fields still require explicit handling. Manual conflict resolution remains an acceptable first-release behavior.

Monitor tombstone and server-history growth; local pruning needs a safe strategy. Backend retention or bootstrap/snapshot APIs require separate backend work because the current contract retains history.

Acceptance: justify normalization with measurements or a specific capability requirement; migrate pending edits, conflicts and opaque records without loss; demonstrate the intended performance benefit. If merging is added, test independent changes and true clashes against retained bases, including deletion and mixed-version data. These are separate deliverables and need not ship together.

### 5. Later product features

Add recurring reminders, standalone alarms, lists/tags, search and keyboard shortcuts incrementally. Specify recurrence timezone/DST behavior, completion and local snooze interaction first. Use explicit compatibility/schema capabilities from 0b; optional fields alone do not make changed behavior safe for older clients. Apply the rollout gate before introducing new record types or incompatible writes.

Acceptance: each feature has defined semantics, migration/backup coverage and mixed-client tests; older supported clients preserve additive fields and cannot perform unsupported behavioral edits. Recurrence and standalone alarms additionally pass device lifecycle, permission and scheduling tests without regressing one-shot reminders.
