# Project development

NothingToDo targets Linux and Android with Tauri 2. Keep domain rules, persistence and sync in `crates/core`; frontend presentation lives in `src`, native adapters in `src-tauri`.

- Use the Astraea Sync contract in `../astraea-sync/README.md`; do not invent backend endpoints or add deployments as part of client changes.
- Keep styling in `src/styles.css` so visual work can proceed independently.
- Preserve offline edits, tombstones and explicit conflict resolution. Never persist account tokens in SQLite, frontend storage or source files.
- Validate relevant changes with `npm run build`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test -p nothingtodo-core`. Run `npm run test:sync` for sync behavior changes when the sibling backend is available.
- Android reminders and alarms are scheduled natively (`Reminders.kt`, `Alarm.kt`) and fire with the app closed; this is emulator-tested, and real-phone testing of the full-screen alarm and notification buttons is still pending. Linux delivery, alarms included, requires the running tray app; say so wherever it matters and do not claim otherwise.
