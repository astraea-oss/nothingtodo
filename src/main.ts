import './styles.css';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification';

interface Task { schemaVersion: number; title: string; notes: string; completed: boolean; remindAt: number | null }
interface Remote { key: string; value: Task | null; version: number; deleted: boolean }
interface TaskRecord { task: Task | null; version: number; dirty: boolean; conflict: Remote | null; notifiedAt: number | null; localRevision: number; snooze: { of: number; until: number } | null; unsupported: { value: unknown; version: number } | null }
interface SyncStatus {
  connected: boolean; remembered: boolean; syncing: boolean; catchingUp: boolean; lastSyncedAt: number | null;
  error: string | null; retryInSeconds: number | null; notice: string | null;
}
interface Snapshot { records: Record<string, TaskRecord>; cursor: number; binding: string | null }

const root = document.querySelector<HTMLDivElement>('#app')!;
root.innerHTML = `
  <main>
    <header class="app-header">
      <div><h1>NothingToDo</h1><p class="tagline">Todos and reminders</p></div>
      <p id="sync-badge" class="badge" role="status" aria-live="polite"></p>
    </header>
    <p id="status" role="status" aria-live="polite"></p>
    <div id="notify-warning" class="warning" role="alert" hidden>
      <p>Notifications are off, so reminders won’t appear.</p>
      <button id="enable-notifications" class="primary" type="button">Turn on notifications</button>
    </div>
    <section class="card" aria-labelledby="editor-heading">
      <h2 id="editor-heading">New task</h2>
      <form id="task-form">
        <label>Title <input name="title" required maxlength="500" autocomplete="off" placeholder="What needs doing?"></label>
        <label>Notes <textarea name="notes" maxlength="8000" rows="2"></textarea></label>
        <label>Remind me <input name="remindAt" type="datetime-local"></label>
        <p class="hint">Dates use this device’s timezone. <span id="delivery-hint"></span></p>
        <div class="actions"><button class="primary" type="submit">Save task</button><button id="cancel-edit" type="button" hidden>Cancel edit</button></div>
      </form>
    </section>
    <section aria-labelledby="list-heading">
      <div class="list-header">
        <h2 id="list-heading">Tasks</h2>
        <label class="inline">Show <select id="filter"><option value="active">Active</option><option value="all">All</option><option value="completed">Completed</option></select></label>
      </div>
      <ul id="tasks"></ul><p id="empty" class="empty" hidden>No tasks here yet.</p>
      <p id="sync-counts" class="hint"></p>
    </section>
    <details class="card" id="settings">
      <summary><h2>Reminders and sync</h2></summary>
      <p class="hint">Sync runs after connecting, after edits, and every 30 seconds while the app is running.</p>
      <form id="sync-form">
        <label>API origin <input name="endpoint" type="url" value="https://api.astraeaoss.co.uk" required></label>
        <label>Account token <input name="token" type="password" autocomplete="off" spellcheck="false" required></label>
        <label class="check"><input name="remember" type="checkbox" checked> Remember on this device (system keyring)</label>
        <button class="primary" type="submit">Connect</button>
      </form>
      <div id="sync-connected" class="actions" hidden>
        <button id="sync" type="button">Sync now</button>
        <button id="disconnect" type="button">Disconnect</button>
      </div>
      <p id="sync-connection" role="status" aria-live="polite"></p>
      <p id="sync-notice" class="notice" role="status"></p>
    </details>
  </main>`;

const $ = <T extends Element>(selector: string) => document.querySelector<T>(selector)!;
const form = $<HTMLFormElement>('#task-form');
const syncForm = $<HTMLFormElement>('#sync-form');
const list = $<HTMLUListElement>('#tasks');
const filter = $<HTMLSelectElement>('#filter');
const cancelEdit = $<HTMLButtonElement>('#cancel-edit');
let data: Snapshot = { records: {}, cursor: 0, binding: null };
let editing: string | null = null;
let busy = false;
let sync: SyncStatus = {
  connected: false, remembered: false, syncing: false, catchingUp: false,
  lastSyncedAt: null, error: null, retryInSeconds: null, notice: null,
};

function showSyncStatus(state: SyncStatus) {
  const rerender = sync.connected !== state.connected;
  sync = state;
  if (rerender) render();
  syncForm.hidden = state.connected;
  $<HTMLElement>('#sync-connected').hidden = !state.connected;
  const message = !state.connected ? 'Not connected. Tasks are stored on this device only.'
    : state.catchingUp ? 'Downloading sync history…'
    : state.syncing ? 'Syncing…'
    : state.error ? `${state.error} Retrying automatically in ${state.retryInSeconds ?? 300}s.`
    : state.lastSyncedAt !== null ? `Last synced ${new Date(state.lastSyncedAt).toLocaleTimeString()}.`
    : 'Connected.';
  const remembered = state.connected ? (state.remembered ? ' Reconnects automatically after restarting.' : ' Connected for this session only.') : '';
  $('#sync-connection').textContent = message + remembered;
  $('#sync-notice').textContent = state.notice ?? '';
  const badge = $<HTMLElement>('#sync-badge');
  badge.textContent = !state.connected ? 'Offline' : state.error ? 'Sync error' : state.syncing || state.catchingUp ? 'Syncing' : 'Synced';
  badge.dataset.state = !state.connected ? 'offline' : state.error ? 'error' : 'ok';
}

async function wakeSync() {
  if (sync.connected) await invoke('sync_now');
}

// Focus can fire many times a minute (with focus-follows-mouse, on every pass of the pointer), so
// returning to the app syncs at most every 15 seconds. The worker's 30-second poll covers the rest.
const RETURN_SYNC_COOLDOWN_MS = 15_000;
let lastReturnSync = 0;
function syncOnReturn() {
  const recent = Math.max(lastReturnSync, sync.lastSyncedAt ?? 0);
  if (sync.syncing || Date.now() - recent < RETURN_SYNC_COOLDOWN_MS) return;
  lastReturnSync = Date.now();
  void wakeSync().catch(error => status(String(error)));
}

function status(message: string) { $('#status').textContent = message; }
function field(name: string) { return form.elements.namedItem(name) as HTMLInputElement | HTMLTextAreaElement; }
function resetEditor() {
  editing = null; form.reset(); cancelEdit.hidden = true;
  $('#editor-heading').textContent = 'New task';
}
function localDate(time: number | null) {
  if (time === null) return '';
  const date = new Date(time);
  return new Date(time - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}
function button(label: string, action: () => Promise<void> | void, className?: string) {
  const element = document.createElement('button');
  element.type = 'button'; element.textContent = label;
  if (className) element.className = className;
  element.addEventListener('click', () => { void run(action); });
  return element;
}
async function run(action: () => Promise<void> | void) {
  if (busy) return;
  busy = true;
  root.setAttribute('aria-busy', 'true');
  const controls = [...root.querySelectorAll<HTMLButtonElement | HTMLInputElement>('button, input[type=checkbox]')].map(b => [b, b.disabled] as const);
  controls.forEach(([b]) => { b.disabled = true; });
  try { await action(); }
  catch (error) { status(String(error)); }
  finally {
    busy = false; root.removeAttribute('aria-busy');
    controls.forEach(([b, disabled]) => { b.disabled = disabled; });
  }
}
async function refresh() {
  data = await invoke<Snapshot>('snapshot');
  render();
}
async function save(id: string, task: Task) {
  await invoke('save_task', { id, task }); await refresh();
}

// When this device will notify: the synced time, or a local snooze of that time. Mirrors the core.
function reminderAt(record: TaskRecord) {
  const remindAt = record.task?.remindAt ?? null;
  const snooze = record.snooze;
  return remindAt !== null && snooze?.of === remindAt && snooze.until > remindAt ? snooze.until : remindAt;
}

// Active first, then soonest reminder, then title.
function compare([, a]: [string, TaskRecord], [, b]: [string, TaskRecord]) {
  const ta = a.task, tb = b.task;
  if (!ta || !tb) return ta ? -1 : tb ? 1 : 0;
  if (ta.completed !== tb.completed) return ta.completed ? 1 : -1;
  const ra = reminderAt(a) ?? Infinity, rb = reminderAt(b) ?? Infinity;
  return ra !== rb ? ra - rb : ta.title.localeCompare(tb.title);
}

function render() {
  list.replaceChildren();
  const now = Date.now();
  for (const [id, record] of Object.entries(data.records).sort(compare)) {
    const task = record.task;
    const readOnly = record.unsupported !== null;
    if (!record.conflict && !readOnly && (!task || (filter.value === 'active' && task.completed) || (filter.value === 'completed' && !task.completed))) continue;
    const li = document.createElement('li');
    li.className = 'task';
    if (task) {
      li.classList.toggle('done', task.completed);
      const head = document.createElement('div'); head.className = 'task-head';
      const check = document.createElement('input');
      check.type = 'checkbox'; check.checked = task.completed; check.disabled = readOnly;
      check.setAttribute('aria-label', task.completed ? `Reopen ${task.title}` : `Complete ${task.title}`);
      check.addEventListener('change', () => { void run(() => save(id, { ...task, completed: check.checked })); });
      const title = document.createElement('h3'); title.textContent = task.title;
      head.append(check, title);
      li.append(head);
      const meta = document.createElement('p'); meta.className = 'meta';
      const due = reminderAt(record);
      if (task.remindAt !== null && due !== null) {
        const reminder = document.createElement('span');
        reminder.className = !task.completed && due <= now ? 'reminder overdue' : 'reminder';
        reminder.textContent = `⏰ ${new Date(task.remindAt).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' })}`;
        meta.append(reminder);
        if (due !== task.remindAt && !task.completed) {
          const snoozed = document.createElement('span');
          snoozed.textContent = `Snoozed on this device until ${new Date(due).toLocaleTimeString([], { timeStyle: 'short' })}`;
          meta.append(snoozed);
        }
      }
      if (record.dirty && sync.connected) {
        const pending = document.createElement('span'); pending.className = 'pending';
        pending.textContent = 'Not yet synced'; meta.append(pending);
      }
      if (readOnly) {
        const note = document.createElement('span'); note.className = 'pending';
        note.textContent = 'Changed by a newer version of NothingToDo. Update the app to edit it.';
        meta.append(note);
      }
      if (meta.childElementCount) li.append(meta);
      if (task.notes) { const notes = document.createElement('p'); notes.className = 'notes'; notes.textContent = task.notes; li.append(notes); }
      const actions = document.createElement('div'); actions.className = 'actions';
      actions.append(button('Edit', () => {
        editing = id; field('title').value = task.title; field('notes').value = task.notes;
        field('remindAt').value = localDate(task.remindAt); cancelEdit.hidden = false;
        $('#editor-heading').textContent = 'Edit task'; field('title').focus();
        form.scrollIntoView({ behavior: 'smooth' });
      }));
      // Only a reminder that has gone off can be snoozed; snoozing earlier would bring it forward.
      if (!task.completed && due !== null && due <= now) actions.append(button('Snooze 10 min', async () => { await invoke('snooze_task', { id, minutes: 10 }); await refresh(); }));
      actions.append(button('Delete', async () => {
        await invoke('delete_task', { id }); if (editing === id) resetEditor(); await refresh();
      }, 'danger'));
      if (!readOnly) li.append(actions);
    } else if (readOnly) {
      const label = document.createElement('h3'); label.textContent = 'A task from a newer version of NothingToDo'; li.append(label);
      const note = document.createElement('p'); note.className = 'meta pending'; note.textContent = 'Update the app to see and edit it.'; li.append(note);
    } else {
      const label = document.createElement('h3'); label.textContent = 'Deleted on this device'; li.append(label);
    }
    if (record.conflict) {
      const conflict = document.createElement('div'); conflict.className = 'conflict';
      const message = document.createElement('p');
      const remote = record.conflict.value;
      message.textContent = `This task was changed on another device. Server version: ${record.conflict.deleted || !remote ? 'deleted' : `“${remote.title}”${remote.completed ? ' (completed)' : ''}`}. Choose which version to keep.`;
      const details = document.createElement('pre'); details.textContent = JSON.stringify(remote, null, 2);
      const actions = document.createElement('div'); actions.className = 'actions';
      actions.append(button('Keep this device’s version', async () => {
        await invoke('resolve_conflict', { id, keepLocal: true }); await refresh();
      }, 'primary'), button('Use server version', async () => {
        await invoke('resolve_conflict', { id, keepLocal: false }); if (editing === id) resetEditor(); await refresh();
      }));
      conflict.append(message, details, actions);
      li.append(conflict);
    }
    list.append(li);
  }
  $<HTMLElement>('#empty').hidden = list.children.length > 0;
  const records = Object.values(data.records);
  const pending = records.filter(r => r.dirty).length, conflicts = records.filter(r => r.conflict).length;
  $('#sync-counts').textContent = sync.connected && (pending || conflicts) ? `${pending} not yet synced · ${conflicts} conflicts` : '';
}

form.addEventListener('submit', event => {
  event.preventDefault();
  void run(async () => {
    const remindAt = field('remindAt').value ? new Date(field('remindAt').value).getTime() : null;
    if (remindAt !== null && !Number.isFinite(remindAt)) throw new Error('Enter a valid reminder date.');
    await invoke('save_task', { id: editing, task: {
      schemaVersion: 1, title: field('title').value, notes: field('notes').value,
      completed: editing ? data.records[editing]?.task?.completed ?? false : false, remindAt,
    } satisfies Task });
    resetEditor(); await refresh(); status('Task saved on this device.');
    // Ask when it first matters; without permission Android drops the reminder silently.
    if (remindAt !== null && !(await isPermissionGranted())) {
      notifyWarning.hidden = await requestPermission() === 'granted';
    }
  });
});
cancelEdit.addEventListener('click', resetEditor);
filter.addEventListener('change', render);
// Android schedules reminders with the OS; desktop shows them from the running app.
$('#delivery-hint').textContent = /Android/i.test(navigator.userAgent)
  ? 'Reminders fire even when the app is closed.'
  : 'Reminders need the app running; closing the window keeps it in the tray.';
const notifyWarning = $<HTMLDivElement>('#notify-warning');
async function checkNotifications() { notifyWarning.hidden = await isPermissionGranted(); }
$('#enable-notifications').addEventListener('click', () => { void run(async () => {
  if (await isPermissionGranted() || await requestPermission() === 'granted') {
    notifyWarning.hidden = true; status('Notifications are on.'); return;
  }
  // After a refusal Android stops showing its prompt; only the app's settings page can help.
  try { await invoke('open_notification_settings'); }
  catch { status('Notifications are blocked. Turn them on for NothingToDo in system settings.'); }
}); });
syncForm.addEventListener('submit', event => {
  event.preventDefault();
  void run(async () => {
    const value = (name: string) => syncForm.elements.namedItem(name) as HTMLInputElement;
    try {
      await invoke('connect_sync', { endpoint: value('endpoint').value, token: value('token').value, remember: value('remember').checked });
      status('Sync connected.');
    } finally { value('token').value = ''; }
  });
});
$('#sync').addEventListener('click', () => { void run(async () => {
  await invoke('sync_now'); status('Sync requested.');
}); });
$('#disconnect').addEventListener('click', () => { void run(async () => {
  await invoke('disconnect_sync');
  status('Sync disconnected. Tasks stay on this device; reconnect with the same account to continue syncing.');
}); });

async function start() {
  if (!isTauri()) {
    status('Open the native app with npm run tauri dev. The browser preview does not store tasks or connect to sync.');
    root.querySelectorAll('input, textarea, button, select').forEach(el => { (el as HTMLInputElement).disabled = true; });
    return;
  }
  await listen('tasks-changed', () => { void refresh().catch(error => status(String(error))); });
  await listen<string>('reminder-error', event => status(event.payload));
  await listen<SyncStatus>('sync-status', event => showSyncStatus(event.payload));
  showSyncStatus(await invoke<SyncStatus>('sync_status'));
  await checkNotifications();
  if (!sync.connected) $<HTMLDetailsElement>('#settings').open = true;
  // Reconnection and foreground signals complement the native worker's periodic retry.
  window.addEventListener('online', () => { void wakeSync().catch(error => status(String(error))); });
  window.addEventListener('focus', syncOnReturn);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState !== 'visible') return;
    syncOnReturn();
    // The user may have changed the permission in system settings, or pressed notification
    // buttons while the app was in the background.
    void checkNotifications();
    void invoke('refresh_reminders');
  });
  await refresh();
  await wakeSync();
}
void start().catch(error => status(String(error)));
