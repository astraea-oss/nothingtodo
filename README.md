# NothingToDo

A local-first todo and reminder app for **Linux and Android**, built with Tauri 2 and Rust. The TypeScript UI is deliberately plain, with styling in `src/styles.css`.

Implemented: create/edit/complete/delete tasks, optional reminder dates, ten-minute snooze, SQLite persistence, automatic Astraea Sync, pending offline changes, and explicit local/server conflict choices.

**Reminders currently run only while the app process is active.** Overdue reminders are checked on restart. This is a working foundation, not yet a reliable background alarm app. The next work is to finalize the app identity, prove Android builds on a phone, and add migrations, recovery and forward-compatible sync. The sync lock fix follows migration support and does not wait for account-identity design. Native local reminders and Android background synchronization have separate checkpoints; see the [roadmap](docs/development.md#roadmap).

## Run on Linux

Install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your distribution, Rust stable, and Node 24 LTS (`.nvmrc`). This machine already has GTK and WebKitGTK installed.

```sh
npm ci
npm run tauri dev
```

`npm run dev` opens only the frontend development server. Use the native Tauri app for persistence, notifications, and sync; the browser page explains this and disables native actions.

```sh
npm run tauri build -- --debug --no-bundle  # local executable
npm run tauri build                       # release packages
```

Click **Enable notifications**, add a reminder a minute in the future, and keep the app open. Desktop notification settings and Do Not Disturb can suppress delivery. Dates are entered/displayed in the device timezone and stored as UTC milliseconds. Delivery receipts are local to each device; completing tasks is synced. An active second device can also notify.

## Android workflow

Use Android Studio to install a JDK, Android SDK, platform tools and NDK. Configure `JAVA_HOME`, `ANDROID_HOME`, and `NDK_HOME` following the [Tauri Android prerequisites](https://v2.tauri.app/start/prerequisites/#android). Then:

```sh
npm run tauri android init
npm run tauri android dev
npm run tauri android build
```

Choose the final app identifier **before running `tauri android init`**; the starter uses `dev.astraea.nothingtodo` and the final identifier has not been selected. Changing it later changes the data directory and Android package identity, so existing data needs an explicit migration. Android project generation has not been completed on this machine because Java/Android tooling is missing. The shared Rust library includes Tauri's mobile entry point. Once generated, commit the Android source project, leaving SDK paths and signing secrets untracked. Test on an actual Android device, including notification permission denial, process death and timezone changes.

A physical phone cannot reach your computer's `localhost` backend. The development sync client permits plain HTTP only to loopback; use `adb reverse tcp:8787 tcp:8787` with the local Worker, or an HTTPS development endpoint. The `nothingtodo` app namespace is the same on both platforms.

## Sync with your backend

The contract comes from [Astraea Sync](https://github.com/astraea-oss/astraea-sync), also checked out at `../astraea-sync`. No changes to the deployed backend are required.

1. Provision a personal token using the backend's documented local setup or private dashboard.
2. Enter the API origin and token in the app. The default origin is `https://api.astraeaoss.co.uk`; local development uses `http://localhost:8787`.
3. Click **Connect for this session** on each device. Sync starts immediately, follows local edits after a short delay, and polls every 30 seconds while active. Returning to the app or regaining connectivity requests an immediate sync. Edits save locally even when offline.
4. When a task conflicts, review the server version and choose **Keep local** or **Use server**. Your choice syncs automatically. **Sync now** remains available to request an immediate attempt.

Failed attempts retry with exponential backoff from 5 seconds up to 5 minutes. Further edits preserve that backoff; returning online, reconnecting or clicking **Sync now** requests an immediate attempt. The sync status shows progress, the last successful sync and failures. Automatic sync requires a running app process; Android may suspend it in the background.

The token is held in Rust memory for the session and cleared from the input after connection. It is never written to SQLite or browser storage. You must reconnect after restarting; automatic sync starts as soon as you reconnect. Never commit tokens or paste them into chat. Protected credential storage is an early milestone: Linux Secret Service with a session-only fallback when unavailable, and token encryption using an Android Keystore-backed key. Plaintext token storage is not a fallback.

A local profile binds to its first successful server/token combination to prevent accidental cross-account uploads. Different devices should currently use the same personal account token. Token rotation, account switching and profile migration need an explicit UI before this restriction can be relaxed. Keep your original token available privately. The stored binding is a SHA-256 fingerprint, not the token itself.

## Checks

```sh
npm run build
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p nothingtodo-core
npm run test:sync -- ../astraea-sync
```

`test:sync` needs the backend's npm dependencies installed. It bundles the backend with Wrangler's **dry-run**, starts an ephemeral local Miniflare/D1 instance, and tests the real HTTP contract with disposable fixture credentials. It never deploys or uses production credentials. Coverage includes two-device conflicts, deletion, pagination beyond 100 changes and authorization failure retention. CI runs the standalone checks and Linux build; the private sibling backend integration is run locally.

## Roadmap and data recovery

The [roadmap](docs/development.md#roadmap) separates app identity/distribution and a real-device Android build (0a) from migrations, compatibility and versioned recovery (0b). The sync lock fix follows the local-revision migration independently of credential design. Packaging targets and performance measurements are chosen early; the encryption-at-rest decision belongs to the credentials milestone.

The first release targets safe persistence/recovery, responsive sync, protected credentials and device-tested one-shot reminders with notification actions. Normalization is conditional on measurements or required capabilities; automatic field merging, recurrence and standalone alarms can follow. Styling and release packaging need not wait for those features.

A compatibility release must reach supported clients before any device writes account markers or incompatible records. Today's clients cannot be made compatible by deleting such records afterward because history retains the original events. Supported additive fields will be preserved on edits; incompatible behavior remains read-only to older clients. Neither policy is implemented yet.

Account-binding migration must verify the original token or an already-established remote identity. If neither is available, preserve local data and block uploads rather than creating a marker in an arbitrary account. Recovery will distinguish restoring the same profile from copying tasks into a different one, including explicit handling of cursors, bindings and device-local alarm state.

Currently, an unsupported task schema or record key stops sync rather than being preserved as opaque data. The database has no numbered migration system or export/import UI yet. Task data is stored unencrypted; full database encryption at rest remains a scheduled decision in the credentials milestone, separate from protected token storage. For now, close the app and copy its entire data directory, including any SQLite WAL files, for a local backup. See [development notes](docs/development.md#boundaries) for its location. Treat backups as private data.

Native Android delivery will not imply continuous sync: periodic background work can be delayed and cannot reproduce the active-process 30-second interval. A device that has not received a completion or deletion can still fire an outdated alarm. The planned first release keeps one-shot reminders at a fixed instant when traveling. Native adapters must define missed-reminder and multiple-device behavior, persist collision-safe notification IDs, reconcile the plugin’s separate schedule, and handle Complete/Snooze actions with the app process stopped. Device-local snooze is proposed to avoid shared reminder changes and unnecessary server history writes; it is not implemented. Background jobs must separately prove safe storage/credential access alongside the foreground worker.
