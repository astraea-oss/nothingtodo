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

**Downgrade protection for today’s binary:** `user_version` alone is insufficient because the current binary never checks it. In the first format migration, move the authoritative snapshot into a new versioned table, retain the legacy `app_state` table with an explicit upgrade-required JSON sentinel that the old `Snapshot` decoder cannot read, and add database triggers rejecting legacy inserts, updates and deletes. Migrate and install these guards atomically; do not leave a readable legacy snapshot that can be saved without new metadata. This is a guarded snapshot relocation, not a requirement to normalize all records now. Require old app instances to be stopped before migration; local database guards cannot revoke network requests from an already-running old process.

Test the actual previous binary against a migrated database: opening must fail clearly, legacy writes (including from a connection opened before migration) must fail, and the new snapshot, revisions and cursor must remain unchanged. Also test rollback after an interrupted migration and that restoring an older backup does not bypass migration/guards on its next supported-client open. Keep an untouched pre-migration backup rather than advertising an in-place downgrade.

**Compatibility semantics:** decode transport envelopes separately from task payloads. Preserve genuinely additive fields in supported records when saving, rather than discarding them or making every extended record read-only. Define compatibility by meaning, not just JSON shape: an optional display field can be additive, while recurrence can alter completion and scheduling semantics even when represented by optional fields. Breaking changes or unsupported behavioral capabilities must be read-only to older clients, using explicit schema/capability rules. Do not assume recurrence belongs in schema 1 merely because its field is optional.

Persist unknown record types and unsupported payloads as opaque data, with server versions and tombstone state, atomically with the page cursor. Continue syncing supported records and show an upgrade indication. Older clients must never overwrite opaque records or drop unknown fields. Specify what happens to pending edits and scheduled alarms when a previously known task becomes unsupported. Test mixed-version pages, unknown tombstones and later upgrades.

**Rollout policy:** ship a compatibility release and establish a minimum supported client version before any supported device writes account markers, new record types or incompatible task data. Today's client rejects non-UUID keys and non-task payloads; a UUID-shaped marker does not avoid payload rejection. Deleting a marker cannot repair old clients because its creation remains in history. Test an actual previous client against an upgraded account and document its required upgrade and recovery behavior. Do not claim transparent compatibility for already-installed code that lacks this machinery.

**Recovery semantics:** export/import must preserve opaque data and exclude credentials. Keep two distinct user operations:

- **Restore the same profile:** validate account identity before uploads; restore the snapshot and its cursor only as a coherent pair. Reconcile with remote changes and tombstones before retrying pending edits, so an old backup cannot silently resurrect a remote deletion. Never advance to a cursor whose page was not applied. Define when to replay from zero instead. Treat a restored account binding as a claim to verify, not authorization to upload.
- **Copy tasks into another profile:** create fresh task IDs, preserve supported task content and additive fields, and start with no inherited server versions, cursor, account binding or pending operations from the source. Do not upload unsupported opaque records into another account; retain them in the original backup and report exclusions.

Device-local receipts, snoozes and notification-ID mappings are not portable by default. Define whether a same-device restore retains or resets them, clear/reconcile stale OS schedules, and make second-device imports apply the missed-reminder policy rather than suppress notifications based on another device's receipts. State these choices in the UI before import.

Acceptance: test migration failure and previous-binary downgrade/write rejection, mixed-version replay and preservation of unknown fields/data; round-trip a versioned backup; restore an old backup after remote deletions; restore pending edits after token rotation; and import onto a second device. Verify cursor/binding handling, receipts, snoozes, alarm mappings and remote writes in each scenario. Failed or unverified imports must preserve existing data and block uploads. Exported copies remain private even when credentials are excluded.

### 1. Responsive sync and worker correctness

Start after the migration/local-revision prerequisite in 0b; do not wait for protected credentials or account marker design. Never hold the store mutex across HTTP calls or the sync-client mutex for an entire attempt. Capture request state, release locks, perform I/O, then commit with revision checks so acknowledgements cannot overwrite concurrent edits. Keep each applied page and its cursor atomic and keep network attempts serialized. Credential replacement must isolate or invalidate responses from the previous session.

Keep manual requests and connection changes responsive, coalesce wake signals, and expose cancellation/progress for long initial downloads. Bound replay work per pass so large histories cannot starve edits or reminder checks. This fix does not depend on normalized SQLite.

Acceptance: deterministic tests cover edits during slow writes, lost acknowledgements, session replacement, offline retries, wake storms and multi-page replay. Test the native worker and frontend busy/status behavior as well as core policy. Compare responsiveness with the 0a baseline; queued network work must not hold local editing or reminder checks behind a request timeout.

### 2. Protected credentials, account migration and encryption decision

Select credential adapters for the initial packaging targets from 0a. Use Linux Secret Service where available, with an explicit session-only fallback on setups without a usable keyring; never persist plaintext tokens. For Flatpak, evaluate the [Secret portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Secret.html): it supplies an app-specific secret for encrypting sandbox data, not a transparent Secret Service API. On Android, encrypt tokens with a Keystore-backed key and store ciphertext privately. Handle unavailable/locked stores, key invalidation, revocation and disconnect without logging secrets.

**Decide encryption at rest by this milestone.** Record whether database encryption is required for the first release or deferred, its threat model, key storage/recovery and backup/export rules. Milestone 0b migrations and the milestone 1 responsiveness fix proceed before this decision and must not wait for it. If encryption is selected here, coordinate key management with credentials and perform a later, numbered encryption migration with its own backup, interruption and key-recovery tests. Combine it with another migration only if that does not delay responsiveness or required protection. SQLCipher is a candidate to evaluate, not a selected dependency. Required protection must not wait for optional normalization. If deferred, state clearly that task data and exports remain plaintext.

**Account identity and migration:** replace the current origin-plus-token fingerprint with a strategy supporting rotation and multiple device tokens without cross-account uploads. The backend has no identity endpoint. A reserved typed marker in the existing namespace remains a candidate, subject to the compatibility rollout in 0b, atomic `baseVersion` initialization and concurrent-device tests. It is not yet a verified design or a reason to invent client endpoints.

For an existing profile, authenticate with the original token and verify its existing origin/token fingerprint before creating or adopting the marker; persist the new identity binding transactionally while preserving pending edits. A replacement token must demonstrate the already-established expected remote identity before any pending upload. Creating a marker in whichever account accepts a replacement token does not establish continuity with the original account.

If the original token is unavailable/revoked and no trustworthy remote identity was established, preserve the local profile, block uploads and offer backup/export or an explicit copy into a separately configured profile. Do not silently rebind. Specify missing, deleted and changed marker behavior: quarantine the connection and require explicit recovery rather than automatically replacing the expected identity. A fresh profile can initialize an identity only through a separately defined onboarding flow.

Acceptance: protected credentials reconnect after restart/resume; session-only fallback works; original-token migration preserves pending edits; same-account token rotation works; wrong-account tokens, revoked originals and missing/deleted/changed markers cannot trigger unsafe uploads. Record and test the encryption decision, key-loss recovery and backup rules for the chosen packaging targets.

### 3a. Native one-shot reminders and notification actions

Define a pure core function for the desired alarm set. Platform adapters reconcile it after edits, completion, deletion, sync, startup and recovery. Define cancellation for unsupported records and unresolved conflicts. Store a collision-safe task UUID to 32-bit notification-ID mapping; do not truncate/hash UUIDs without collision handling. Support mapping recovery and cancellation when restoring a profile.

**Mandatory native execution and storage foundation:** notification actions with the process stopped require safe native storage access even when periodic network sync is omitted. Own that foundation here: select a single-owner or transaction/reload strategy shared by foreground code, action receivers and recovery jobs, with no dependency on an open webview. Current compare-and-swap detects stale writers but does not coordinate Store instances or processes. Prove durable local mutation, credential/key-unavailable handling where relevant, process restart and foreground/action overlap. Persist shared completion as a pending sync change without requiring a network request; persist local snooze and reconcile the OS alarm before reporting action success. Milestone 3b reuses this foundation rather than introducing it.

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

Acceptance: prove the native storage ownership strategy with foreground/action overlap, duplicate action delivery, restart and unavailable keys; test reconciliation in core and permission denial/revocation, sleep/wake, process death, notification actions with the process stopped, reboot/unlock, missed first-sync alarms, mapping collisions, backup recovery and timezone/DST changes on devices. Local one-shot alarm acceptance is distinct from background cross-device sync acceptance.

### 3b. Optional Android background network sync checkpoint

Extend the mandatory native execution/storage foundation accepted in 3a to periodic network work. Before committing to an adapter, prove a background job can obtain protected credentials, perform sync and reconcile OS schedules without a webview, using the same storage ownership strategy as foreground code and notification actions. Test credential/session changes and foreground/action/job overlap. Making this network feature optional does not make cold-process action handling or safe native storage coordination optional.

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

### 5a. Entry types, recurrence and warnings

Planned, not implemented. Designed on 2026-10-28 in `docs/design/ui-directions.html` (tabs 02 "Entry types" and 03 "Adding"). This milestone defines the recurrence and standalone-alarm work from milestone 5. It depends on the compatibility and rollout rules from 0b and the native execution foundation from 3a.

**Entry model.** Each task gains a `kind`:

- **Alarm:** needs an immediate response. Requires a time. Rings until the user stops it (press-and-hold), with one-tap snooze.
- **Reminder:** worth a nudge at the time. Requires a time. Plays one sound at the time, then stays as a notification. Existing tasks migrate to reminders.
- **Marker** (working name): holidays, birthdays and deadlines. No time, just a local date or a span of dates. Silent on the day itself.

Fields:

- **Required:** title, kind and when. The kind is guessed from input and the user can change it. For alarms and reminders, "when" is an instant; for markers it is a local date or date range.
- **Optional:**
  - duration;
  - one repeat rule;
  - any number of warnings before the entry;
  - `notes`, which replaces the separate description and notes ideas. Its first line serves as the summary.
- **Sound:** follows the kind. A per-entry override can come later.

Repeat presets are 8 hours, daily, weekly, 28 days, 1, 3, 4, 6 and 12 months, and custom. Daily and weekly repeats take an optional set of weekdays, so an alarm can run Monday to Friday; "weekdays", "weekends" and "every day" are shortcuts for that set. Warnings are offsets such as 5 minutes, 1 day or 1 week. Markers can also use "morning of", a local time defaulting to 09:00.

**Engineering requirements:**

- **Schema.** Kind, date-only "when", duration, repeat rules and warnings change behaviour, not just display. Introduce them as task schema version 2 using the capability rules from 0b.
  - Supported clients that don't understand version 2 keep those records read-only and preserve their fields.
  - Migrate version 1 tasks to reminders, keeping `notes`.
  - Publish a minimum client version before any device writes version 2 records.
- **Time semantics.** Fixed intervals (8 hours, 28 days) are exact durations from the anchor instant. Calendar repeats (daily, weekly, months, yearly), including any weekday set, use local wall-clock time in a stored IANA timezone, so they keep their local time across DST and travel. This is the floating-time model that one-shot reminders deliberately lack.
  - Compute each occurrence from the anchor, not from the previous occurrence. That way a monthly repeat clamped to 28 February returns to the 31st in March.
  - Define leap-day behaviour for yearly repeats.
  - Markers are local dates and never shift with the timezone.
- **Series editing and completion.** Define these before building the UI:
  - completing one occurrence versus ending the series;
  - editing "this occurrence" versus "this and following";
  - how deletions and tombstones apply to single occurrences;
  - how conflicting edits to a repeat rule are resolved. Explicit resolution is the default, as in milestone 4.
- **Scheduling.** The 3a desired-alarm function expands occurrences × warnings over a bounded horizon and re-expands as occurrences pass.
  - Each warning is its own notification with its own ID, so the collision-safe ID mapping must cover (task, occurrence, warning).
  - Android limits how many alarms an app can have pending. Keep the horizon and alarm count well under that limit.
  - Apply the missed-reminder policy per occurrence, so an app closed for a day doesn't fire a backlog of 8-hourly doses.
- **Alarm delivery.** Ringing until stopped is a separate native path from the notification plugin's one-shot schedule.
  - **Android:** use an alarm-clock alarm (`AlarmManager.setAlarmClock`) and a full-screen alert that loops audio on the alarm stream until stopped or snoozed.
  - **Android 14+ permission:** full-screen intents are a special permission there. Check `NotificationManager.canUseFullScreenIntent()` and fall back to a heads-up notification when it is denied.
  - **Exact alarms:** the exact-alarm permission rules from 3a still apply.
  - **Linux:** the tray process plays the sound until stopped. Nothing rings when the app isn't running, and the UI must say so.
- **Markers.** Markers schedule nothing for the day itself. Their warnings are ordinary silent or one-sound notifications at the chosen local time.
- **Cross-device behaviour.** Snooze stays device-local. Stopping an alarm on one device doesn't stop another device that is already ringing; document this together with the stale-alarm limits in 3b.

Acceptance:

- **Core tests:** recurrence expansion across DST changes, month ends and leap years; warning expansion and horizon limits; version 1 to 2 migration; mixed-client replay where version 1 clients stay read-only.
- **Device tests:** alarms ringing with the screen off, the phone locked and the process killed; full-screen permission denied; reboot; backlog suppression after the app has been closed.
- **Regressions:** none in one-shot reminders.

### 5b. Interface direction and adding entries

Planned, not implemented. Agreed on 2026-10-28 from `docs/design/ui-directions.html`, which records the options considered and the "Decided" slide. Styling stays in `src/styles.css`. The adding fields depend on 5a; the list and strip can start earlier with today's reminders.

**Visual language.** Follows Nothing's style: black and white, a dot-matrix font for numerals and headings, dot textures and dot instruments instead of icons.

- Red is reserved for now, overdue, conflicts and a ringing alarm. It appears once as the signature dot on the Add button.
- Entry types are shapes (tab 02 · A): alarm a filled double ring, reminder a hollow ring, marker a square.
- No free-time visualisation anywhere.

**Main view (leading direction: 02 Strip, tab 01).**

- A week strip of square day cells: weekday letter top left, date bottom right, load dots below, no outlines on unselected days.
- Below it, upcoming entries as spreadsheet-like rows (time, shape, title, time until), grouped by day under plain day headings with no borders.
- Markers sit at the top of their day as all-day rows. Alarms carry a small RINGS tag.

**Adding (tab 03: B structure, B1 tiles, B2 instruments).**

- **Short sheet:** title, round type keys (alarm, reminder, marker), the time in a dot readout, day chips (Today, tomorrow, the next day, Pick), a dot grip and Add. Most entries finish here: a title and a time.
- **Pulled up:** every other field as a widget tile with big dot numerals. When, Duration, Repeat, Warn before, Sound, Snooze, Type and Notes. The title is the heading. Pull down to return to the short sheet.
- **Tile edit:** tapping a tile opens it in place at full width with its presets while the others dim. There is no separate picker screen.
- **Instruments (from B2):** drawn inside tiles where they help: a 24-hour dot dial for When, a dot bar for Duration, a dot timeline for Warn before, a dot ring for Repeat showing one cycle with occurrences lit, and a dot waveform for Sound.
- **Repeat tile:** presets 8 h, Daily, Weekly, 28 d, 1, 3, 4, 6 and 12 months, and Custom. Daily and Weekly show day keys (M–S) with Weekdays, Weekends and All shortcuts, so an alarm can run Monday to Friday. The tile shows the next occurrences.
- **Warn before tile:** pick any number of warnings (5 min to 1 month, plus "Day of" for markers), shown on a timeline before the start.
- **Tiles follow the type:** markers have no Sound or Snooze tile; alarms ring until stopped.
- **Typing stays a shortcut:** typed tokens such as `fri 8am`, `for 45m`, `weekdays 7am` and `warn 1w` fill the same tiles. Templates (recipes) are set aside and may return as saved presets.
- **Notes** replaces the separate description and notes fields. Its first line shows as a summary under the title.
- **Priority** describes how the three types differ. It is not a field.

Acceptance: an entry with a title and time takes one line and one tap; every 5a field is reachable from the pulled-up sheet without leaving it; tiles show only the fields that apply to the type; the layout works one-handed on a phone and with a keyboard on Linux; light and dark themes both keep red for its reserved meanings.
