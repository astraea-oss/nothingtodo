import '@fontsource/doto/700.css';
import '@fontsource/doto/900.css';
import '@fontsource/space-grotesk/400.css';
import '@fontsource/space-grotesk/600.css';
import '@fontsource/space-mono/400.css';
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
      <div>
        <h1 id="date-heading" class="dot"></h1>
        <p id="date-detail" class="label"></p>
      </div>
      <button id="sync-badge" class="badge" type="button" aria-label="Sync and backup settings" aria-live="polite"></button>
    </header>
    <div id="notify-warning" class="warning" role="alert" hidden>
      <p>Notifications are off, so reminders won’t appear.</p>
      <button id="enable-notifications" class="primary" type="button">Turn on notifications</button>
    </div>
    <p id="status" role="status" aria-live="polite"></p>
    <nav class="week" aria-label="Week">
      <button id="week-back" class="icon" type="button" aria-label="Previous week">‹</button>
      <div id="strip" class="strip"></div>
      <button id="week-next" class="icon" type="button" aria-label="Next week">›</button>
    </nav>
    <div class="list-header">
      <p id="list-label" class="label"></p>
      <input id="search" type="search" placeholder="Search" aria-label="Search titles and notes (press /)" autocomplete="off">
      <select id="filter" aria-label="Show"><option value="active">Upcoming</option><option value="all">All</option><option value="completed">Done</option></select>
    </div>
    <div id="tasks"></div>
    <p id="empty" class="empty" hidden>Nothing here yet.</p>
    <p id="sync-counts" class="hint"></p>
    <button id="add" class="add" type="button"><span class="red-dot" aria-hidden="true"></span>Add</button>
    <div id="sheet" class="sheet" role="dialog" aria-modal="true" aria-labelledby="sheet-title" hidden>
      <form id="task-form">
        <div class="grip" aria-hidden="true"></div>
        <input id="sheet-title" name="title" class="sheet-title" required maxlength="500" autocomplete="off" placeholder="What needs doing?" aria-label="Title">
        <div class="keys" role="radiogroup" aria-label="Type">
          <button type="button" class="key" disabled title="Alarms arrive with entry types"><i class="shape alarm"></i>Alarm</button>
          <button type="button" class="key on" aria-pressed="true"><i class="shape reminder"></i>Reminder</button>
          <button type="button" class="key" disabled title="Markers arrive with entry types"><i class="shape marker"></i>Marker</button>
        </div>
        <div class="readout">
          <input name="time" type="time" class="dot" aria-label="Time">
          <span id="readout-day" class="label"></span>
        </div>
        <div id="day-chips" class="chips" role="radiogroup" aria-label="Day"></div>
        <input name="date" type="date" hidden aria-label="Date">
        <button id="more" type="button" class="link" aria-expanded="false">More ▴</button>
        <div id="more-fields" hidden>
          <label>Notes <textarea name="notes" maxlength="8000" rows="3"></textarea></label>
        </div>
        <p class="hint">Dates use this device’s timezone. <span id="delivery-hint"></span></p>
        <div class="actions">
          <button class="primary" type="submit"><span class="red-dot" aria-hidden="true"></span><span id="sheet-submit">Add</span></button>
          <button id="cancel-edit" type="button">Cancel</button>
        </div>
      </form>
    </div>
    <div id="settings" class="sheet" role="dialog" aria-modal="true" aria-labelledby="settings-heading" hidden>
     <div class="panel">
      <div class="panel-head"><h2 id="settings-heading" class="label">Settings</h2><button id="close-settings" class="icon" type="button" aria-label="Close">×</button></div>
      <section>
      <h3 class="label">Sync</h3>
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
      </section>
      <section>
      <h3 class="label">Backup</h3>
      <p class="hint">Backups hold your tasks in plain text, without your sync token. Keep them private.</p>
      <div class="actions">
        <button id="export-backup" type="button">Export backup</button>
        <button id="choose-backup" type="button">Use a backup file…</button>
        <input id="backup-file" type="file" accept=".json,application/json" hidden>
      </div>
      <div id="backup-choice" class="conflict" hidden>
        <p id="backup-summary"></p>
        <div class="actions">
          <button id="backup-copy" type="button">Add these tasks to this device</button>
          <button id="backup-restore" type="button">Replace this device’s tasks</button>
          <button id="backup-cancel" type="button">Cancel</button>
        </div>
        <p class="hint">Adding creates new copies, safe for another account. Replacing restores this profile as it was; the current data is saved first, and changes made elsewhere since the backup sync back in.</p>
      </div>
      </section>
     </div>
    </div>
  </main>`;

const $ = <T extends Element>(selector: string) => document.querySelector<T>(selector)!;
const form = $<HTMLFormElement>('#task-form');
const syncForm = $<HTMLFormElement>('#sync-form');
const list = $<HTMLDivElement>('#tasks');
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
function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = '', text = '') {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
}
function button(label: string, action: () => Promise<void> | void, className?: string) {
  const element = el('button', className, label);
  element.type = 'button';
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

// ── Days. Local midnights, stepped with Date so DST changes do not shift them. ──
function startOfDay(time: number) { const d = new Date(time); d.setHours(0, 0, 0, 0); return d.getTime(); }
function addDays(day: number, count: number) { const d = new Date(day); d.setDate(d.getDate() + count); return d.getTime(); }
function weekStart(day: number) { const d = new Date(day); d.setDate(d.getDate() - (d.getDay() + 6) % 7); return d.getTime(); }
function isoWeek(day: number) {
  const d = new Date(day); d.setDate(d.getDate() + 3 - (d.getDay() + 6) % 7);
  const first = new Date(d.getFullYear(), 0, 4);
  return 1 + Math.round(((d.getTime() - first.getTime()) / 86_400_000 - 3 + (first.getDay() + 6) % 7) / 7);
}
function dayName(day: number) {
  const today = startOfDay(Date.now());
  if (day === today) return 'Today';
  if (day === addDays(today, 1)) return 'Tomorrow';
  if (day === addDays(today, -1)) return 'Yesterday';
  return new Date(day).toLocaleDateString([], { weekday: 'short', day: 'numeric', month: 'short' });
}
// 24-hour, as in the design: fixed width, so rows line up.
const clock = (time: number) => new Date(time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
function until(due: number, now: number) {
  const minutes = Math.round((due - now) / 60_000);
  if (minutes < 0) return 'late';
  if (minutes < 1) return 'now';
  if (minutes < 60) return `${minutes}m`;
  if (minutes < 48 * 60) return `${Math.round(minutes / 60)}h`;
  return `${Math.round(minutes / 1440)}d`;
}
function isoDate(day: number) {
  const d = new Date(day), p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

let selectedDay = startOfDay(Date.now());
let expanded: string | null = null;
type Entry = [string, TaskRecord];

function renderHeader() {
  const day = new Date(selectedDay);
  $('#date-heading').textContent = `${day.toLocaleDateString([], { weekday: 'short' })} ${day.getDate()}`;
  $('#date-detail').textContent = `${day.toLocaleDateString([], { month: 'short', year: 'numeric' })} · wk ${isoWeek(selectedDay)}`;
  const strip = $<HTMLDivElement>('#strip');
  strip.replaceChildren();
  const today = startOfDay(Date.now());
  const start = weekStart(selectedDay);
  for (let i = 0; i < 7; i++) {
    const day = addDays(start, i), next = addDays(day, 1);
    const load = Object.values(data.records).filter(r => {
      const due = reminderAt(r);
      return r.task && !r.task.completed && due !== null && due >= day && due < next;
    }).length;
    const cell = el('button', 'day');
    cell.type = 'button';
    cell.classList.toggle('on', day === selectedDay);
    cell.classList.toggle('today', day === today);
    cell.setAttribute('aria-label', `${new Date(day).toDateString()}, ${load} reminder${load === 1 ? '' : 's'}`);
    cell.setAttribute('aria-pressed', String(day === selectedDay));
    cell.append(el('span', 'label', new Date(day).toLocaleDateString([], { weekday: 'narrow' })), el('b', 'dot', String(new Date(day).getDate())));
    const dots = el('span', 'load');
    for (let n = 0; n < Math.min(load, 4); n++) dots.append(el('i'));
    cell.append(dots);
    cell.addEventListener('click', () => { selectedDay = day; render(); });
    strip.append(cell);
  }
}

function row(id: string, record: TaskRecord, now: number) {
  const task = record.task;
  const readOnly = record.unsupported !== null;
  const due = reminderAt(record);
  const wrap = el('div', 'entry');
  const line = el('div', 'row');
  line.classList.toggle('done', !!task?.completed);
  line.classList.toggle('overdue', !!task && !task.completed && due !== null && due <= now);
  const tick = el('input', 'tick');
  tick.type = 'checkbox'; tick.checked = !!task?.completed; tick.disabled = readOnly || !task;
  tick.setAttribute('aria-label', task?.completed ? `Reopen ${task.title}` : `Complete ${task?.title ?? 'task'}`);
  if (task) tick.addEventListener('change', () => { void run(() => save(id, { ...task, completed: tick.checked })); });
  const title = el('button', 'title');
  title.type = 'button';
  title.setAttribute('aria-expanded', String(expanded === id));
  title.append(el('span', '', task?.title ?? (readOnly ? 'A task from a newer version' : 'Deleted on this device')));
  const firstLine = task?.notes.split('\n')[0];
  if (firstLine && expanded !== id) title.append(el('span', 'sub', firstLine));
  title.addEventListener('click', () => { expanded = expanded === id ? null : id; render(); });
  const glyph = el('i', record.conflict ? 'glyph conflict' : record.dirty && sync.connected ? 'glyph pending' : 'glyph');
  glyph.title = record.conflict ? 'Conflict' : record.dirty && sync.connected ? 'Not yet synced' : '';
  line.append(tick, el('span', 'time', due !== null ? clock(due) : '—'), el('i', 'shape reminder'), title,
    el('span', 'until', task?.completed ? 'done' : due !== null ? until(due, now) : ''), glyph);
  wrap.append(line);
  if (expanded === id || record.conflict) wrap.append(details(id, record, now));
  return wrap;
}

function details(id: string, record: TaskRecord, now: number) {
  const task = record.task;
  const readOnly = record.unsupported !== null;
  const due = reminderAt(record);
  const box = el('div', 'details');
  const meta = el('p', 'meta');
  if (task?.remindAt != null) meta.append(el('span', '', `${new Date(task.remindAt).toLocaleDateString([], { dateStyle: 'full' })}, ${clock(task.remindAt)}`));
  if (task && due !== null && due !== task.remindAt && !task.completed) meta.append(el('span', '', `Snoozed on this device until ${clock(due)}`));
  if (record.dirty && sync.connected) meta.append(el('span', 'pending', 'Not yet synced'));
  if (readOnly) meta.append(el('span', 'pending', 'Changed by a newer version of NothingToDo. Update the app to edit it.'));
  if (meta.childElementCount) box.append(meta);
  if (task?.notes) box.append(el('p', 'notes', task.notes));
  if (task && !readOnly) {
    const actions = el('div', 'actions');
    actions.append(button('Edit', () => openSheet(id)));
    // Only a reminder that has gone off can be snoozed; snoozing earlier would bring it forward.
    if (!task.completed && due !== null && due <= now) actions.append(button('Snooze 10 min', async () => { await invoke('snooze_task', { id, minutes: 10 }); await refresh(); }));
    actions.append(button('Delete', async () => { await invoke('delete_task', { id }); expanded = null; await refresh(); }, 'quiet'));
    box.append(actions);
  }
  if (record.conflict) {
    const conflict = el('div', 'conflict');
    const remote = record.conflict.value;
    conflict.append(el('p', '', `Changed on another device. Server version: ${record.conflict.deleted || !remote ? 'deleted' : `“${remote.title}”${remote.completed ? ' (completed)' : ''}`}. Choose which to keep.`));
    const actions = el('div', 'actions');
    actions.append(button('Keep this device’s version', async () => {
      await invoke('resolve_conflict', { id, keepLocal: true }); await refresh();
    }, 'primary'), button('Use server version', async () => {
      await invoke('resolve_conflict', { id, keepLocal: false }); await refresh();
    }));
    conflict.append(actions);
    box.append(conflict);
  }
  return box;
}

function group(label: string, entries: Entry[], now: number, options: { id?: string; className?: string; empty?: string } = {}) {
  const section = el('section', `group ${options.className ?? ''}`);
  if (options.id) section.id = options.id;
  section.append(el('h2', 'day-heading', label));
  for (const [id, record] of entries) section.append(row(id, record, now));
  if (!entries.length && options.empty) section.append(el('p', 'nothing', options.empty));
  list.append(section);
}

function render() {
  renderHeader();
  list.replaceChildren();
  const now = Date.now();
  const view = filter.value;
  const byTime = (a: Entry, b: Entry) => (reminderAt(a[1]) ?? Infinity) - (reminderAt(b[1]) ?? Infinity) || (a[1].task?.title ?? '').localeCompare(b[1].task?.title ?? '');
  const entries = (Object.entries(data.records) as Entry[]).sort(byTime);
  // Conflicts and records from newer versions always show, whatever the filter.
  const attention = entries.filter(([, r]) => r.conflict || (r.unsupported && !r.task));
  const query = search.value.trim().toLowerCase();
  const matches = (task: Task) => !query || task.title.toLowerCase().includes(query) || task.notes.toLowerCase().includes(query);
  const shown = entries.filter(e => !attention.includes(e) && e[1].task && matches(e[1].task) &&
    (view === 'all' || (view === 'completed') === e[1].task.completed));
  if (attention.length) group('Needs attention', attention, now, { className: 'attention' });
  if (view === 'active') {
    const overdue = shown.filter(e => reminderAt(e[1])! <= now && reminderAt(e[1]) !== null);
    if (overdue.length) group('Overdue', overdue, now, { className: 'late' });
  }
  const timed = shown.filter(e => reminderAt(e[1]) !== null && (view !== 'active' || reminderAt(e[1])! > now));
  const days = new Map<number, Entry[]>();
  for (const entry of timed) {
    const day = startOfDay(reminderAt(entry[1])!);
    // Searching looks across every day, not just from the selected one.
    if (view === 'active' && !query && day < selectedDay) continue;
    days.set(day, [...(days.get(day) ?? []), entry]);
  }
  if (view === 'active' && !query && !days.has(selectedDay)) days.set(selectedDay, []);
  for (const day of [...days.keys()].sort((a, b) => a - b)) {
    group(dayName(day), days.get(day)!, now, { id: `day-${day}`, className: day === selectedDay ? 'selected' : '', empty: 'Nothing planned.' });
  }
  const untimed = shown.filter(e => reminderAt(e[1]) === null);
  if (untimed.length) group('No time', untimed, now);
  $('#list-label').textContent = query ? 'Matches' : view === 'active' ? `From ${dayName(selectedDay).toLowerCase()}` : view === 'all' ? 'Everything' : 'Done';
  if (query && !shown.length) list.append(el('p', 'nothing', `Nothing matches “${search.value.trim()}”.`));
  $<HTMLElement>('#empty').hidden = entries.some(e => e[1].task || e[1].conflict || e[1].unsupported);
  const records = Object.values(data.records);
  const pending = records.filter(r => r.dirty).length, conflicts = records.filter(r => r.conflict).length;
  $('#sync-counts').textContent = sync.connected && (pending || conflicts) ? `${pending} not yet synced · ${conflicts} conflicts` : '';
}

// ── The add sheet ──
const sheet = $<HTMLDivElement>('#sheet');
function chooseDay(day: number) {
  field('date').value = isoDate(day);
  $('#readout-day').textContent = dayName(day);
  const pick = field('date') as HTMLInputElement;
  for (const chip of $<HTMLDivElement>('#day-chips').children) {
    chip.setAttribute('aria-pressed', String((chip as HTMLElement).dataset.day === String(day)));
  }
  pick.hidden = [...$<HTMLDivElement>('#day-chips').children].some(c => (c as HTMLElement).dataset.day === String(day));
}
function renderChips() {
  const chips = $<HTMLDivElement>('#day-chips');
  chips.replaceChildren();
  const today = startOfDay(Date.now());
  for (const day of [today, addDays(today, 1), addDays(today, 2)]) {
    const chip = el('button', 'chip', day === addDays(today, 2) ? new Date(day).toLocaleDateString([], { weekday: 'short' }) : dayName(day));
    chip.type = 'button'; chip.dataset.day = String(day);
    chip.addEventListener('click', () => chooseDay(day));
    chips.append(chip);
  }
  const pick = el('button', 'chip', 'Pick…');
  pick.type = 'button';
  pick.addEventListener('click', () => { const input = field('date') as HTMLInputElement; input.hidden = false; input.showPicker?.(); input.focus(); });
  chips.append(pick);
}
function openSheet(id: string | null) {
  editing = id;
  form.reset();
  renderChips();
  const task = id ? data.records[id]?.task : null;
  const at = task?.remindAt ?? null;
  field('title').value = task?.title ?? '';
  field('notes').value = task?.notes ?? '';
  field('time').value = at !== null ? new Date(at).toTimeString().slice(0, 5) : '';
  chooseDay(at !== null ? startOfDay(at) : Math.max(selectedDay, startOfDay(Date.now())));
  const more = !!task?.notes;
  $<HTMLElement>('#more-fields').hidden = !more;
  $('#more').setAttribute('aria-expanded', String(more));
  $('#sheet-submit').textContent = id ? 'Save' : 'Add';
  sheet.hidden = false;
  field('title').focus();
}
function resetEditor() { editing = null; form.reset(); sheet.hidden = true; }
$('#add').addEventListener('click', () => openSheet(null));
$('#more').addEventListener('click', () => {
  const fields = $<HTMLElement>('#more-fields');
  fields.hidden = !fields.hidden;
  $('#more').setAttribute('aria-expanded', String(!fields.hidden));
});
field('date').addEventListener('change', () => {
  const value = field('date').value;
  if (value) chooseDay(startOfDay(new Date(`${value}T00:00`).getTime()));
});
sheet.addEventListener('click', event => { if (event.target === sheet) resetEditor(); });
// Sync and backup live in a sheet opened from the sync label, out of the main view.
const settings = $<HTMLDivElement>('#settings');
const closeSettings = () => { settings.hidden = true; };
$('#sync-badge').addEventListener('click', () => { settings.hidden = false; });
$('#close-settings').addEventListener('click', closeSettings);
settings.addEventListener('click', event => { if (event.target === settings) closeSettings(); });
const search = $<HTMLInputElement>('#search');
search.addEventListener('input', render);
// Keyboard: n adds, / searches, Escape closes the sheet or clears the search.
document.addEventListener('keydown', event => {
  const typing = event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement || event.target instanceof HTMLSelectElement;
  if (event.key === 'Escape') {
    if (!settings.hidden) closeSettings();
    else if (!sheet.hidden) resetEditor();
    else if (search.value) { search.value = ''; render(); search.blur(); }
    return;
  }
  if (typing || event.ctrlKey || event.metaKey || event.altKey || !sheet.hidden || !settings.hidden) return;
  if (event.key === 'n') { event.preventDefault(); openSheet(null); }
  if (event.key === '/') { event.preventDefault(); search.focus(); }
});
$('#week-back').addEventListener('click', () => { selectedDay = addDays(selectedDay, -7); render(); });
$('#week-next').addEventListener('click', () => { selectedDay = addDays(selectedDay, 7); render(); });

form.addEventListener('submit', event => {
  event.preventDefault();
  void run(async () => {
    const time = field('time').value, date = field('date').value;
    const remindAt = time ? new Date(`${date}T${time}`).getTime() : null;
    if (remindAt !== null && !Number.isFinite(remindAt)) throw new Error('Enter a valid date and time.');
    const existing = editing ? data.records[editing]?.task : null;
    await invoke('save_task', { id: editing, task: {
      ...existing, schemaVersion: 1, title: field('title').value, notes: field('notes').value,
      completed: existing?.completed ?? false, remindAt,
    } satisfies Task });
    if (remindAt !== null) selectedDay = Math.max(startOfDay(Date.now()), startOfDay(remindAt));
    resetEditor(); await refresh(); status('Saved on this device.');
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
// Local time for backup file names, e.g. 2026-10-29-0915.
function stamp() {
  const d = new Date(), p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
}
interface BackupSummary { tasks: number; skipped: number; exportedAt: number }
const backupFile = $<HTMLInputElement>('#backup-file');
const backupChoice = $<HTMLDivElement>('#backup-choice');
let chosenBackup: string | null = null;
function closeBackupChoice() { chosenBackup = null; backupFile.value = ''; backupChoice.hidden = true; }
$('#export-backup').addEventListener('click', () => { void run(async () => {
  status(`Backup saved to ${await invoke<string>('export_backup', { stamp: stamp() })}.`);
}); });
$('#choose-backup').addEventListener('click', () => backupFile.click());
backupFile.addEventListener('change', () => { void run(async () => {
  const file = backupFile.files?.[0];
  if (!file) return;
  const json = await file.text();
  const preview = await invoke<BackupSummary & { currentTasks: number }>('preview_backup', { json });
  chosenBackup = json;
  const when = new Date(preview.exportedAt).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
  const skipped = preview.skipped ? ` ${preview.skipped} item(s) from a newer app version are kept on restore but cannot be added as copies.` : '';
  $('#backup-summary').textContent = `Backup from ${when} with ${preview.tasks} task(s). This device has ${preview.currentTasks}.${skipped}`;
  backupChoice.hidden = false;
}); });
$('#backup-cancel').addEventListener('click', closeBackupChoice);
$('#backup-copy').addEventListener('click', () => { void run(async () => {
  if (!chosenBackup) return;
  const done = await invoke<BackupSummary>('copy_backup', { json: chosenBackup });
  closeBackupChoice(); await refresh();
  status(`Added ${done.tasks} task(s) from the backup${done.skipped ? `; left out ${done.skipped} from a newer app version` : ''}.`);
}); });
$('#backup-restore').addEventListener('click', () => { void run(async () => {
  if (!chosenBackup) return;
  const done = await invoke<BackupSummary & { previous: string }>('restore_backup', { json: chosenBackup, stamp: stamp() });
  closeBackupChoice(); await refresh();
  status(`Restored ${done.tasks} task(s). The data it replaced was saved to ${done.previous}.`);
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
