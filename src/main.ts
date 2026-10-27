import './styles.css';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification';

interface Task { schemaVersion: number; title: string; notes: string; completed: boolean; remindAt: number | null }
interface Remote { key: string; value: Task | null; version: number; deleted: boolean }
interface TaskRecord { task: Task | null; version: number; dirty: boolean; conflict: Remote | null; notifiedAt: number | null }
interface SyncStatus { connected: boolean; syncing: boolean; lastSyncedAt: number | null; error: string | null; retryInSeconds: number | null }
interface Snapshot { records: Record<string, TaskRecord>; cursor: number; binding: string | null }

const root = document.querySelector<HTMLDivElement>('#app')!;
root.innerHTML = `
  <main>
    <header><h1>NothingToDo</h1><p>Todos and reminders</p></header>
    <p id="status" role="status" aria-live="polite"></p>
    <section aria-labelledby="editor-heading">
      <h2 id="editor-heading">New task</h2>
      <form id="task-form">
        <label>Title <input name="title" required maxlength="500" autocomplete="off"></label>
        <label>Notes <textarea name="notes" maxlength="8000" rows="3"></textarea></label>
        <label>Remind me <input name="remindAt" type="datetime-local"></label>
        <p class="hint">Dates use this device’s timezone. Reminders currently require the app to be running.</p>
        <div class="actions"><button type="submit">Save task</button><button id="cancel-edit" type="button" hidden>Cancel edit</button></div>
      </form>
    </section>
    <section aria-labelledby="list-heading">
      <h2 id="list-heading">Tasks</h2>
      <label>Show <select id="filter"><option value="active">Active</option><option value="all">All</option><option value="completed">Completed</option></select></label>
      <ul id="tasks"></ul><p id="empty" hidden>No tasks here yet.</p>
    </section>
    <section aria-labelledby="settings-heading">
      <h2 id="settings-heading">Reminders and sync</h2>
      <button id="notifications" type="button">Enable notifications</button>
      <p class="hint">Sync runs automatically after connecting, after edits, and every 30 seconds while active. Reconnect after restarting; your token stays in memory for this session.</p>
      <form id="sync-form">
        <label>API origin <input name="endpoint" type="url" value="https://api.astraeaoss.co.uk" required></label>
        <label>Account token <input name="token" type="password" autocomplete="off" spellcheck="false" required></label>
        <button type="submit">Connect for this session</button>
      </form>
      <button id="sync" type="button" disabled>Sync now</button>
      <p id="sync-status" role="status"></p>
      <p id="sync-connection" role="status" aria-live="polite"></p>
    </section>
  </main>`;

const form = document.querySelector<HTMLFormElement>('#task-form')!;
const list = document.querySelector<HTMLUListElement>('#tasks')!;
const filter = document.querySelector<HTMLSelectElement>('#filter')!;
const syncButton = document.querySelector<HTMLButtonElement>('#sync')!;
const cancelEdit = document.querySelector<HTMLButtonElement>('#cancel-edit')!;
let data: Snapshot = { records: {}, cursor: 0, binding: null };
let editing: string | null = null;
let busy = false;
let syncConnected = false;

function showSyncStatus(state: SyncStatus) {
  syncConnected = state.connected;
  syncButton.disabled = busy || !state.connected;
  const message = !state.connected ? 'Connect to enable automatic sync.'
    : state.syncing ? 'Syncing…'
    : state.error ? `${state.error} Retrying automatically in up to ${state.retryInSeconds ?? 300}s.`
    : state.lastSyncedAt !== null ? `Last synced ${new Date(state.lastSyncedAt).toLocaleTimeString()}. Automatic sync is on.`
    : 'Automatic sync is ready.';
  document.querySelector('#sync-connection')!.textContent = message;
}

async function wakeSync() {
  if (syncConnected) await invoke('sync_now');
}

function status(message: string) { document.querySelector('#status')!.textContent = message; }
function field(name: string) { return form.elements.namedItem(name) as HTMLInputElement | HTMLTextAreaElement; }
function resetEditor() {
  editing = null; form.reset(); cancelEdit.hidden = true;
  document.querySelector('#editor-heading')!.textContent = 'New task';
}
function localDate(time: number | null) {
  if (time === null) return '';
  const date = new Date(time);
  return new Date(time - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}
function button(label: string, action: () => Promise<void> | void) {
  const element = document.createElement('button');
  element.type = 'button'; element.textContent = label;
  element.addEventListener('click', () => { void run(action); });
  return element;
}
async function run(action: () => Promise<void> | void) {
  if (busy) return;
  busy = true;
  root.setAttribute('aria-busy', 'true');
  const controls = [...root.querySelectorAll<HTMLButtonElement>('button')].map(b => [b, b.disabled] as const);
  controls.forEach(([b]) => { b.disabled = true; });
  try { await action(); }
  catch (error) { status(String(error)); }
  finally {
    busy = false; root.removeAttribute('aria-busy');
    controls.forEach(([b, disabled]) => { b.disabled = disabled; });
    syncButton.disabled = !syncConnected;
  }
}
async function refresh() {
  data = await invoke<Snapshot>('snapshot');
  render();
}
async function save(id: string, task: Task) {
  await invoke('save_task', { id, task }); await refresh();
}
function render() {
  list.replaceChildren();
  const entries = Object.entries(data.records).sort(([a], [b]) => a.localeCompare(b));
  for (const [id, record] of entries) {
    const task = record.task;
    if (!record.conflict && (!task || (filter.value === 'active' && task.completed) || (filter.value === 'completed' && !task.completed))) continue;
    const li = document.createElement('li');
    if (task) {
      const title = document.createElement('h3'); title.textContent = task.title; li.append(title);
      const meta = document.createElement('p');
      meta.textContent = `${task.completed ? 'Completed' : 'Active'} · ${record.dirty ? 'Pending sync' : 'Synced'}${task.remindAt !== null ? ` · Reminder: ${new Date(task.remindAt).toLocaleString()}` : ''}`;
      li.append(meta);
      if (task.notes) { const notes = document.createElement('p'); notes.className = 'notes'; notes.textContent = task.notes; li.append(notes); }
      const actions = document.createElement('div'); actions.className = 'actions';
      actions.append(button(task.completed ? 'Reopen' : 'Complete', () => save(id, { ...task, completed: !task.completed })), button('Edit', () => {
        editing = id; field('title').value = task.title; field('notes').value = task.notes;
        field('remindAt').value = localDate(task.remindAt); cancelEdit.hidden = false;
        document.querySelector('#editor-heading')!.textContent = 'Edit task'; field('title').focus();
      }));
      if (!task.completed && task.remindAt !== null) actions.append(button('Snooze 10 min', () => save(id, { ...task, remindAt: Date.now() + 600_000 })));
      actions.append(button('Delete', async () => {
        await invoke('delete_task', { id }); if (editing === id) resetEditor(); await refresh();
      }));
      li.append(actions);
    } else {
      const label = document.createElement('h3'); label.textContent = 'Locally deleted task'; li.append(label);
    }
    if (record.conflict) {
      const conflict = document.createElement('div'); conflict.className = 'conflict';
      const message = document.createElement('p');
      message.textContent = `Sync conflict. Server version: ${record.conflict.deleted ? 'deleted' : record.conflict.value?.title}. Choose which version to keep. Your choice will sync automatically.`;
      const remote = document.createElement('pre'); remote.textContent = JSON.stringify(record.conflict.value, null, 2);
      conflict.append(message, remote, button('Keep local', async () => {
        await invoke('resolve_conflict', { id, keepLocal: true }); await refresh();
      }), button('Use server', async () => {
        await invoke('resolve_conflict', { id, keepLocal: false }); if (editing === id) resetEditor(); await refresh();
      }));
      li.append(conflict);
    }
    list.append(li);
  }
  document.querySelector<HTMLElement>('#empty')!.hidden = list.children.length > 0;
  const records = Object.values(data.records);
  document.querySelector('#sync-status')!.textContent = `${records.filter(r => r.dirty).length} pending · ${records.filter(r => r.conflict).length} conflicts`;
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
  });
});
cancelEdit.addEventListener('click', resetEditor);
filter.addEventListener('change', render);
document.querySelector('#notifications')!.addEventListener('click', () => { void run(async () => {
  const granted = await isPermissionGranted() || await requestPermission() === 'granted';
  status(granted ? 'Notifications enabled. Keep the app running for reminders.' : 'Notifications are blocked. Check system settings.');
}); });
document.querySelector<HTMLFormElement>('#sync-form')!.addEventListener('submit', event => {
  event.preventDefault();
  const syncForm = event.currentTarget as HTMLFormElement;
  void run(async () => {
    const token = syncForm.elements.namedItem('token') as HTMLInputElement;
    try {
      await invoke('connect_sync', { endpoint: (syncForm.elements.namedItem('endpoint') as HTMLInputElement).value, token: token.value });
      syncConnected = true;
      status('Automatic sync connected for this session.');
    } finally { token.value = ''; }
  }).then(() => { if (syncConnected) syncButton.disabled = false; });
});
syncButton.addEventListener('click', () => { void run(async () => {
  await invoke('sync_now'); status('Sync requested.');
}); });

async function start() {
  if (!isTauri()) {
    status('Open the native app with npm run tauri dev. The browser preview does not store tasks or connect to sync.');
    root.querySelectorAll('input, textarea, button, select').forEach(el => { (el as HTMLInputElement).disabled = true; });
    return;
  }
  await listen('tasks-changed', () => { void refresh().catch(error => status(String(error))); });
  await listen<string>('reminder-error', event => status(`Could not save reminder receipt: ${event.payload}`));
  await listen<SyncStatus>('sync-status', event => showSyncStatus(event.payload));
  showSyncStatus(await invoke<SyncStatus>('sync_status'));
  // Reconnection and foreground signals complement the native worker's periodic retry.
  window.addEventListener('online', () => { void wakeSync().catch(error => status(String(error))); });
  window.addEventListener('focus', () => { void wakeSync().catch(error => status(String(error))); });
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') void wakeSync().catch(error => status(String(error)));
  });
  await refresh();
  await wakeSync();
}
void start().catch(error => status(String(error)));
