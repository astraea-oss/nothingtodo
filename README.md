# NothingToDo

A local-first todo and reminder app for **Linux and Android**, built with Tauri 2 and Rust. The TypeScript UI is deliberately plain, with styling in `src/styles.css`.

Implemented: create/edit/complete/delete tasks, optional reminder dates, ten-minute snooze that stays on the device (other devices keep the original time), SQLite persistence with numbered migrations, automatic Astraea Sync that never blocks editing on the network, credentials remembered in the Linux system keyring or Android Keystore, pending offline changes, and explicit local/server conflict choices.

**On Android, reminders are scheduled with the system** and fire with the app closed and after a reboot. **On Linux they need the app running:** closing the window keeps it in the tray so reminders still fire; **Quit** in the tray menu stops them. Reminders already past when a device learns about them (after a first sync, or with the app closed for a while) show as overdue rather than all notifying at once. A device that has not yet synced a completion elsewhere can still fire an outdated reminder. Notification Complete/Snooze buttons and a release signing key come next. Native local reminders and Android background synchronization have separate checkpoints; see the [roadmap](docs/development.md#roadmap).

## Run on Linux

Install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your distribution, Rust stable, and Node 24 LTS (`.nvmrc`). On Arch/CachyOS:

```sh
sudo pacman -S --needed webkit2gtk-4.1 gtk3 libayatana-appindicator base-devel
npm ci
npm run tauri dev
```

`npm run dev` opens only the frontend development server. Use the native Tauri app for persistence, notifications, and sync; the browser page explains this and disables native actions.

### Portable package

```sh
npm run package:linux   # release/NothingToDo-<version>-linux-<arch>.tar.gz
```

Extract the archive anywhere writable and run `nothingtodo`. Tasks are stored in a `NothingToDo Data` folder beside the executable, so moving or backing up the folder moves the tasks; close the app first, because SQLite WAL files can hold recent edits. Development builds keep theirs in `target/debug/NothingToDo Data`. Sync credentials are never in that folder: they live in the system keyring (Secret Service, e.g. gnome-keyring or KWallet).

- `./install.sh` adds a launcher entry and icons that point at the folder where it sits; nothing is copied. Run it again after moving the folder.
- `./install.sh --autostart` also adds a systemd user service that starts NothingToDo hidden in the tray at login (`nothingtodo --hidden`).
- `./install.sh --uninstall` removes the launcher, icons and service and leaves the data alone.
- `NothingToDo-safe.sh` starts the app with `WEBKIT_DISABLE_DMABUF_RENDERER=1` for GPUs or drivers that show a blank window.

Starting the app again while it runs shows the existing window. Without a tray host (StatusNotifierItem), closing the window quits.

Click **Enable notifications**, add a reminder a minute in the future, and keep the app running (open or in the tray). Desktop notification settings and Do Not Disturb can suppress delivery. Dates are entered/displayed in the device timezone and stored as UTC milliseconds. Delivery receipts are local to each device; completing tasks is synced. An active second device can also notify.

## Android

Needs a JDK 17, the Android SDK (platform 36+, build tools, platform tools) and the NDK, plus `rustup target add aarch64-linux-android`. The scripts use `JAVA_HOME`, `ANDROID_HOME` and `NDK_HOME` when set, otherwise `~/Android/jdk17`, `~/Android/Sdk` and the newest `~/Android/Sdk/ndk/*`.

```sh
npm run package:android   # release/NothingToDo-<version>-android-arm64.apk (optimized)
adb install -r release/NothingToDo-0.1.0-android-arm64.apk
```

Or copy the APK to the phone and open it (allow installing from that app when asked). For a development build with WebView debugging, export the three variables above and run `npm run tauri android dev` or `npm run tauri android build -- --debug --apk --target aarch64`.

**Signing.** Without `src-tauri/gen/android/keystore.properties` the APK is signed with this machine's Android debug key: fine for testing, but Android only installs updates signed with the same key, and switching to a release key later means uninstalling first. Before sharing builds, create a release keystore outside the repository, back it up, and point `keystore.properties` at it (`storeFile`, `storePassword`, `keyAlias`, `keyPassword`; the file is git-ignored).

The app identifier `uk.co.astraeaoss.nothingtodo` is also the Android package name and must not change. The generated project in `src-tauri/gen/android` is committed; local SDK paths and signing secrets are ignored. On Android, tasks live in the app's private storage and remembered sync credentials are encrypted with an Android Keystore key. Upcoming reminders (up to 64 at a time) are handed to Android as exact alarms through `Reminders.kt`, so they fire with the app closed; a boot receiver re-arms them after a restart or app update. Exact timing uses the `USE_EXACT_ALARM` permission, granted at install for sideloaded builds.

A phone cannot reach your computer's `localhost`. The client allows plain HTTP only to loopback, so either use the HTTPS API or forward a local Worker with `adb reverse tcp:8787 tcp:8787`.

## Sync with your backend

The contract comes from [Astraea Sync](https://github.com/astraea-oss/astraea-sync), also checked out at `../astraea-sync`. No changes to the deployed backend are required.

1. Provision a personal token using the backend's documented local setup or private dashboard.
2. Enter the API origin and token in the app. The default origin is `https://api.astraeaoss.co.uk`; local development uses `http://localhost:8787`.
3. Click **Connect** on each device. Leave **Remember on this device** checked to keep the credentials in the system keyring and reconnect automatically after restarting. Sync starts immediately, follows local edits after a short delay, and polls every 30 seconds while active. Returning to the app or regaining connectivity requests an immediate sync. Edits save locally even when offline.
4. When a task conflicts, review the server version and choose **Keep this device’s version** or **Use server version**. Your choice syncs automatically. **Sync now** requests an immediate attempt; **Disconnect** stops syncing and removes remembered credentials, keeping tasks on the device.

Failed attempts retry with exponential backoff from 5 seconds up to 5 minutes. Further edits preserve that backoff; returning online, reconnecting or clicking **Sync now** requests an immediate attempt. The sync status shows progress, the last successful sync and failures. Automatic sync requires a running app process; Android may suspend it in the background.

On Linux, a remembered token and API origin are stored in the Secret Service (GNOME Keyring, KWallet or another provider). Without a usable keyring, or with **Remember** unchecked, the token is held in Rust memory for the session only and the app says so. The input is cleared after connecting. Tokens are never written to SQLite, browser storage or logs, and plaintext storage is not a fallback. Android has no credential adapter yet, so it stays session-only until the Keystore-backed adapter exists. The keyring only saves the token; it does not check that it works, so a mistyped token is remembered until you disconnect. Never commit tokens or paste them into chat.

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

The [roadmap](docs/development.md#roadmap) separates app identity/distribution and a real-device Android build (0a) from migrations, compatibility and versioned recovery (0b). The sync lock fix follows the local-revision migration independently of credential design. Packaging targets and performance measurements are chosen early; the encryption-at-rest decision belongs to the credentials milestone. After the first release, milestone 5a adds entry types (alarm, reminder, marker), recurrence and warnings, and 5b the new interface; the options considered and the agreed direction are in the [UI directions deck](docs/design/ui-directions.html).

The first release targets safe persistence/recovery, responsive sync, protected credentials and device-tested one-shot reminders with notification actions. Normalization is conditional on measurements or required capabilities; automatic field merging, recurrence and standalone alarms can follow. Styling and release packaging need not wait for those features.

A compatibility release must reach supported clients before any device writes account markers or incompatible records. Today's clients cannot be made compatible by deleting such records afterward because history retains the original events. Supported additive fields will be preserved on edits; incompatible behavior remains read-only to older clients. Neither policy is implemented yet.

Account-binding migration must verify the original token or an already-established remote identity. If neither is available, preserve local data and block uploads rather than creating a marker in an arbitrary account. Recovery will distinguish restoring the same profile from copying tasks into a different one, including explicit handling of cursors, bindings and device-local alarm state.

Currently, an unsupported task schema or record key stops sync rather than being preserved as opaque data. The database is migrated in numbered steps; before upgrading an older profile the app writes an untouched `nothingtodo.sqlite3.backup-v<N>-<time>` copy beside it, and binaries from before migration support can no longer open or write the upgraded profile. Downgrading means restoring that backup with the app closed, which loses later edits. There is no export/import UI yet. Task data is stored unencrypted; full database encryption at rest remains a scheduled decision in the credentials milestone, separate from protected token storage. For now, close the app and copy its entire data directory, including any SQLite WAL files, for a local backup. See [development notes](docs/development.md#boundaries) for its location. Treat backups as private data.

Native Android delivery will not imply continuous sync: periodic background work can be delayed and cannot reproduce the active-process 30-second interval. A device that has not received a completion or deletion can still fire an outdated alarm. The planned first release keeps one-shot reminders at a fixed instant when traveling. Native adapters must define missed-reminder and multiple-device behavior, persist collision-safe notification IDs, reconcile the plugin’s separate schedule, and handle Complete/Snooze actions with the app process stopped. Device-local snooze is proposed to avoid shared reminder changes and unnecessary server history writes; it is not implemented. Safe native storage coordination with the foreground worker is mandatory for cold-process notification actions. Optional background network jobs must reuse that foundation and separately prove credential access and synchronization.
