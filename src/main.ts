import '@fontsource/doto/700.css';
import '@fontsource/doto/900.css';
import '@fontsource/space-grotesk/400.css';
import '@fontsource/space-grotesk/600.css';
import '@fontsource/space-mono/400.css';
import '@fontsource/space-mono/700.css';
import './styles.css';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isPermissionGranted, requestPermission } from '@tauri-apps/plugin-notification';

// Layout follows direction 02 "Strip" in docs/design/ui-directions.html: a dot date header, a week
// strip with dot loads, day bands with rows on a dotted gutter rail and a red NOW line, and an
// input bar at the bottom. Keep additions inside that structure.

// Schema 2 adds `kind` and, for markers, a local `date`; the core picks the schema when saving.
interface Task { schemaVersion: number; title: string; notes: string; completed: boolean; remindAt: number | null; kind?: 'reminder' | 'marker' | 'alarm'; date?: string }
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
    <header class="top">
      <h1 id="date-heading" class="dot"></h1>
      <button id="open-settings" class="lbl" type="button" aria-label="Settings"></button>
    </header>
    <div id="strip" class="strip" aria-label="Week"></div>
    <div id="gutter" class="gt"></div>
    <div class="fx-wrap">
      <form id="quick" class="fx">
        <button id="open-sheet" class="fx-k" type="button" aria-label="More fields">ƒx</button>
        <input id="quick-input" class="fx-in" autocomplete="off" enterkeyhint="done" aria-label="New task">
      </form>
      <div class="fx-hint">
        <button id="unscheduled" class="lbl" type="button"></button>
        <span id="hint" class="lbl" role="status" aria-live="polite"></span>
      </div>
    </div>
    <div id="sheet" class="sheet" role="dialog" aria-modal="true" aria-labelledby="sheet-title" hidden>
      <form id="task-form" class="panel">
        <div class="grip" aria-hidden="true"></div>
        <input id="sheet-title" name="title" class="sheet-title" required maxlength="500" autocomplete="off" placeholder="What needs doing?" aria-label="Title">
        <div class="keys" role="radiogroup" aria-label="Type">
          <button type="button" class="key" disabled title="Alarms arrive with entry types"><i class="node al"></i>Alarm</button>
          <button type="button" class="key on" data-kind="reminder" aria-pressed="true"><i class="node"></i>Reminder</button>
          <button type="button" class="key" data-kind="marker" aria-pressed="false"><i class="node mk"></i>Marker</button>
        </div>
        <div class="readout">
          <input name="time" type="time" class="dot" aria-label="Time">
          <span id="readout-day" class="lbl"></span>
        </div>
        <div id="day-chips" class="chips" role="radiogroup" aria-label="Day"></div>
        <input name="date" type="date" hidden aria-label="Date">
        <label class="lbl">Notes <textarea name="notes" maxlength="8000" rows="3"></textarea></label>
        <p id="delivery-hint" class="note"></p>
        <div class="row-actions">
          <button class="pill on" type="submit"><span id="sheet-submit">↵ Add</span></button>
          <button id="cancel-edit" class="pill" type="button">Cancel</button>
        </div>
      </form>
    </div>
    <div id="settings" class="sheet" role="dialog" aria-modal="true" aria-labelledby="settings-heading" hidden>
      <div class="panel">
        <div class="panel-head"><h2 id="settings-heading" class="lbl">Settings</h2><button id="close-settings" class="pill" type="button">Close</button></div>
        <section>
          <h3 class="lbl">Sync</h3>
          <form id="sync-form">
            <label class="lbl">API origin <input name="endpoint" type="url" value="https://api.astraeaoss.co.uk" required></label>
            <label class="lbl">Account token <input name="token" type="password" autocomplete="off" spellcheck="false" required></label>
            <label class="check"><input name="remember" type="checkbox" checked> Remember on this device (system keyring)</label>
            <button class="pill on" type="submit">Connect</button>
          </form>
          <div id="sync-connected" class="row-actions" hidden>
            <button id="sync" class="pill" type="button">Sync now</button>
            <button id="disconnect" class="pill" type="button">Disconnect</button>
          </div>
          <p id="sync-connection" class="note" role="status" aria-live="polite"></p>
          <p id="sync-notice" class="note" role="status"></p>
        </section>
        <section>
          <h3 class="lbl">Backup</h3>
          <p class="note">Backups hold your tasks in plain text, without your sync token. Keep them private.</p>
          <div class="row-actions">
            <button id="export-backup" class="pill" type="button">Export</button>
            <button id="choose-backup" class="pill" type="button">Use a backup file</button>
            <input id="backup-file" type="file" accept=".json,application/json" hidden>
          </div>
          <div id="backup-choice" class="conflict" hidden>
            <p id="backup-summary" class="note"></p>
            <div class="row-actions">
              <button id="backup-copy" class="pill" type="button">Add as copies</button>
              <button id="backup-restore" class="pill r" type="button">Replace this device’s tasks</button>
              <button id="backup-cancel" class="pill" type="button">Cancel</button>
            </div>
            <p class="note">Copies are safe for another account. Replacing restores this profile; the current data is saved first, and changes made elsewhere since sync back in.</p>
          </div>
        </section>
      </div>
    </div>
  </main>`;

const $ = <T extends Element>(selector: string) => document.querySelector<T>(selector)!;
const form = $<HTMLFormElement>('#task-form');
const syncForm = $<HTMLFormElement>('#sync-form');
const gutter = $<HTMLDivElement>('#gutter');
const quick = $<HTMLInputElement>('#quick-input');
const cancelEdit = $<HTMLButtonElement>('#cancel-edit');
const sheet = $<HTMLDivElement>('#sheet');
const settings = $<HTMLDivElement>('#settings');
let data: Snapshot = { records: {}, cursor: 0, binding: null };
let editing: string | null = null;
let busy = false;
let notificationsOff = false;
let sync: SyncStatus = {
  connected: false, remembered: false, syncing: false, catchingUp: false,
  lastSyncedAt: null, error: null, retryInSeconds: null, notice: null,
};

// ── Small helpers ──
function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = '', text = '') {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
}
function pill(label: string, action: () => Promise<void> | void, className = '') {
  const element = el('button', `pill ${className}`, label);
  element.type = 'button';
  element.addEventListener('click', event => { event.stopPropagation(); void run(action); });
  return element;
}
function field(name: string) { return form.elements.namedItem(name) as HTMLInputElement | HTMLTextAreaElement; }

// Messages replace the right-hand hint under the input bar for a few seconds.
let statusTimer = 0;
function status(message: string) {
  const hint = $<HTMLElement>('#hint');
  hint.textContent = message;
  hint.classList.add('message');
  clearTimeout(statusTimer);
  statusTimer = window.setTimeout(() => { hint.classList.remove('message'); renderHint(); }, 5000);
}

async function run(action: () => Promise<void> | void) {
  if (busy) return;
  busy = true;
  root.setAttribute('aria-busy', 'true');
  try { await action(); }
  catch (error) { status(String(error)); }
  finally { busy = false; root.removeAttribute('aria-busy'); }
}
async function refresh() {
  data = await invoke<Snapshot>('snapshot');
  render();
}
async function save(id: string | null, task: Task) {
  await invoke('save_task', { id, task }); await refresh();
}

// When this device will notify: the synced time, or a local snooze of that time. Mirrors the core.
function reminderAt(record: TaskRecord) {
  const remindAt = record.task?.remindAt ?? null;
  const snooze = record.snooze;
  return remindAt !== null && snooze?.of === remindAt && snooze.until > remindAt ? snooze.until : remindAt;
}

// ── Days: local midnights, stepped with Date so DST changes do not shift them ──
function startOfDay(time: number) { const d = new Date(time); d.setHours(0, 0, 0, 0); return d.getTime(); }
function addDays(day: number, count: number) { const d = new Date(day); d.setDate(d.getDate() + count); return d.getTime(); }
function weekStart(day: number) { const d = new Date(day); d.setDate(d.getDate() - (d.getDay() + 6) % 7); return d.getTime(); }
function isoWeek(day: number) {
  const d = new Date(day); d.setDate(d.getDate() + 3 - (d.getDay() + 6) % 7);
  const first = new Date(d.getFullYear(), 0, 4);
  return 1 + Math.round(((d.getTime() - first.getTime()) / 86_400_000 - 3 + (first.getDay() + 6) % 7) / 7);
}
const two = (n: number) => String(n).padStart(2, '0');
const weekday = (day: number) => new Date(day).toLocaleDateString([], { weekday: 'short' });
const shortDay = (day: number) => `${weekday(day)} ${new Date(day).getDate()}`;
const longDay = (day: number) => `${weekday(day)} ${new Date(day).getDate()} ${new Date(day).toLocaleDateString([], { month: 'short' })}`;
const clock = (time: number) => { const d = new Date(time); return `${two(d.getHours())}:${two(d.getMinutes())}`; };
function isoDate(day: number) { const d = new Date(day); return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`; }
// Countdown as in the design: 45m, 1h14, 1d19, with a minus sign once past.
function countdown(due: number, now: number) {
  const minutes = Math.round((due - now) / 60_000), sign = minutes < 0 ? '−' : '', m = Math.abs(minutes);
  if (m < 60) return `${sign}${m}m`;
  if (m < 1440) return `${sign}${Math.floor(m / 60)}h${two(m % 60)}`;
  return `${sign}${Math.floor(m / 1440)}d${two(Math.floor((m % 1440) / 60))}`;
}

let selectedDay = startOfDay(Date.now());
let expanded: string | null = null;
let showUnscheduled = false;
type Entry = [string, TaskRecord];

// ── Typing in the bar: "Call the bank fri 8am" ──
const DAYS = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];
const FULL_DAYS = ['sunday', 'monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday'];
interface Parsed { title: string; day: number | null; minutes: number | null }
function parse(text: string): Parsed {
  let day: number | null = null, minutes: number | null = null;
  const today = startOfDay(Date.now());
  const kept: string[] = [];
  for (const word of text.trim().split(/\s+/).filter(Boolean)) {
    const w = word.toLowerCase();
    // A time needs am/pm or a separator, so "2 bins" stays in the title.
    const time = /^(\d{1,2})(?:[:.](\d{2}))?(am|pm)?$/.exec(w);
    if (time && (time[2] || time[3]) && minutes === null) {
      let hour = Number(time[1]);
      if (time[3] === 'pm' && hour < 12) hour += 12;
      if (time[3] === 'am' && hour === 12) hour = 0;
      const minute = Number(time[2] ?? 0);
      if (hour < 24 && minute < 60) { minutes = hour * 60 + minute; continue; }
    }
    if (day === null && (w === 'today' || w === 'tod')) { day = today; continue; }
    if (day === null && (w === 'tomorrow' || w === 'tmr' || w === 'tom')) { day = addDays(today, 1); continue; }
    const index = Math.max(DAYS.indexOf(w), FULL_DAYS.indexOf(w));
    if (day === null && index >= 0) { day = addDays(today, (index - new Date(today).getDay() + 7) % 7); continue; }
    if (w === 'at' && kept.length) continue;
    kept.push(word);
  }
  return { title: kept.join(' '), day, minutes };
}
// Markers are local dates: midnight of that day here, whatever the time zone.
function markerDay(record: TaskRecord) {
  const date = record.task?.kind === 'marker' ? record.task.date : undefined;
  return date ? new Date(`${date}T00:00`).getTime() : null;
}
function quickTarget(parsed: Parsed) {
  if (parsed.minutes === null) return null;
  const d = new Date(parsed.day ?? selectedDay);
  d.setHours(Math.floor(parsed.minutes / 60), parsed.minutes % 60, 0, 0);
  return d.getTime();
}

// ── Rendering ──
function renderHeader(shownDay: number, preview: boolean) {
  $('#date-heading').textContent = shortDay(shownDay);
  const label = $<HTMLButtonElement>('#open-settings');
  label.classList.toggle('red', preview || !!sync.error);
  label.textContent = preview ? 'preview' : sync.error ? 'sync error ›'
    : `${new Date(shownDay).toLocaleDateString([], { month: 'short', year: 'numeric' })} · wk ${isoWeek(shownDay)}`;
  const strip = $<HTMLDivElement>('#strip');
  strip.replaceChildren();
  const now = Date.now();
  const start = weekStart(shownDay);
  const month = new Date(shownDay).getMonth();
  for (let i = 0; i < 7; i++) {
    const day = addDays(start, i), next = addDays(day, 1);
    const cell = el('button', day === shownDay ? 'on' : '');
    cell.type = 'button';
    const open = Object.values(data.records).filter(r => r.task && !r.task.completed && !r.unsupported);
    const dues = open.map(reminderAt).filter((d): d is number => d !== null && d >= day && d < next);
    const marks = open.filter(r => markerDay(r) === day).length;
    const count = dues.length + marks;
    cell.setAttribute('aria-label', `${new Date(day).toDateString()}, ${count} entr${count === 1 ? 'y' : 'ies'}`);
    cell.setAttribute('aria-pressed', String(day === shownDay));
    const load = el('span', 'load');
    const dots = [...Array(marks).fill(''), ...dues.map(due => (due <= now ? 'r' : ''))];
    for (const dot of dots.slice(0, 4)) load.append(el('i', dot));
    cell.append(el('span', 'lbl', new Date(day).toLocaleDateString([], { weekday: 'narrow' })),
      el('b', new Date(day).getMonth() !== month ? 'dim' : '', two(new Date(day).getDate())), load);
    cell.dataset.day = String(day);
    cell.addEventListener('click', () => { selectedDay = day; expanded = null; render(); });
    strip.append(cell);
  }
}

function band(label: string, count: string, className = '', onClick?: () => void) {
  const b = el(onClick ? 'button' : 'div', `gt-band ${className}`);
  if (onClick) { (b as HTMLButtonElement).type = 'button'; b.addEventListener('click', onClick); }
  b.append(el('span', '', label), el('span', '', count));
  gutter.append(b);
}

function nowLine(now: number) {
  const line = el('div', 'gt-now');
  line.append(el('span', 'tm', clock(now)), el('span', 'nd'), el('hr'));
  gutter.append(line);
}

function row(id: string, record: TaskRecord, now: number, showDay = false) {
  const task = record.task;
  const readOnly = record.unsupported !== null;
  const due = reminderAt(record);
  const marker = task?.kind === 'marker';
  const done = !!task?.completed;
  const late = !done && due !== null && due <= now;
  const line = el('div', 'gt-row');
  line.classList.toggle('open', expanded === id);
  // The node on the rail is the checkbox; markers are squares.
  const node = el('button', `node${marker ? ' mk' : ''}${done ? ' x' : ''}${late ? ' r' : ''}`);
  node.type = 'button';
  node.disabled = readOnly || !task;
  node.setAttribute('aria-label', done ? `Reopen ${task?.title}` : `Complete ${task?.title ?? 'task'}`);
  if (task) node.addEventListener('click', event => { event.stopPropagation(); void run(() => save(id, { ...task, completed: !done })); });
  const markedDay = markerDay(record);
  const time = marker && markedDay !== null ? (showDay ? shortDay(markedDay) : 'all day')
    : due === null ? '—' : showDay ? `${weekday(due)} ${clock(due)}` : clock(due);
  const title = el('span', `t${done ? ' done' : ''}`, task?.title ?? (readOnly ? 'From a newer app version' : 'Deleted on this device'));
  // Sync glyphs: synced is silent, pending a ring, conflict a red dot.
  const sy = el('span', record.conflict ? 'sy c' : record.dirty && sync.connected ? 'sy p' : 'sy');
  line.append(el('span', `tm${marker ? ' ad' : ''}${late ? ' red' : done ? ' dim' : ''}`, time), node, title,
    el('span', `n${late ? ' red' : ''}`, done ? 'done' : due !== null ? countdown(due, now) : ''), sy);
  line.addEventListener('click', () => { if (lifted) return; expanded = expanded === id ? null : id; render(); });
  if (task && !readOnly && (task.remindAt !== null || marker)) holdToMove(line, id);
  gutter.append(line);
  if (expanded === id || record.conflict) gutter.append(details(id, record, now));
}

function details(id: string, record: TaskRecord, now: number) {
  const task = record.task;
  const readOnly = record.unsupported !== null;
  const due = reminderAt(record);
  const box = el('div', 'gt-detail');
  if (task?.notes) box.append(el('p', 'notes', task.notes));
  if (task && due !== null && due !== task.remindAt && !task.completed) box.append(el('p', 'lbl', `Snoozed on this device until ${clock(due)}`));
  if (record.dirty && sync.connected) box.append(el('p', 'lbl', 'Not yet synced'));
  if (readOnly) box.append(el('p', 'lbl', 'Changed by a newer app version. Update to edit.'));
  if (task && !readOnly) {
    const actions = el('div', 'row-actions');
    actions.append(pill('Edit', () => openSheet(id)));
    // Only a reminder that has gone off can be snoozed; snoozing earlier would bring it forward.
    if (!task.completed && due !== null && due <= now) actions.append(pill('Snooze 10m', async () => { await invoke('snooze_task', { id, minutes: 10 }); await refresh(); }));
    actions.append(pill('Delete', async () => { await invoke('delete_task', { id }); expanded = null; await refresh(); }));
    box.append(actions);
  }
  if (record.conflict) {
    const remote = record.conflict.value;
    const conflict = el('div', 'conflict');
    conflict.append(el('p', 'note', `Changed on another device: ${record.conflict.deleted || !remote ? 'deleted there' : `“${remote.title}”${remote.completed ? ', done' : ''}`}. Keep which?`));
    const actions = el('div', 'row-actions');
    actions.append(pill('This device', async () => { await invoke('resolve_conflict', { id, keepLocal: true }); await refresh(); }, 'on'),
      pill('Other device', async () => { await invoke('resolve_conflict', { id, keepLocal: false }); await refresh(); }));
    conflict.append(actions);
    box.append(conflict);
  }
  box.addEventListener('click', event => event.stopPropagation());
  return box;
}

function render() {
  const parsed = parse(quick.value);
  const target = quickTarget(parsed);
  // Typing a day previews it, as in the design: the strip jumps there and the new row shows red.
  const previewDay = quick.value.trim() && parsed.day !== null ? parsed.day : null;
  const shownDay = previewDay ?? selectedDay;
  renderHeader(shownDay, previewDay !== null);

  gutter.replaceChildren();
  const now = Date.now();
  const today = startOfDay(now);
  const entries = (Object.entries(data.records) as Entry[])
    .sort((a, b) => (reminderAt(a[1]) ?? Infinity) - (reminderAt(b[1]) ?? Infinity) || (a[1].task?.title ?? '').localeCompare(b[1].task?.title ?? ''));
  if (notificationsOff) band('Notifications off · turn on ›', '', 'red', () => { void run(enableNotifications); });

  // Records needing a decision always show, whatever the day.
  const attention = entries.filter(([, r]) => r.conflict || (r.unsupported && !r.task));
  if (attention.length) {
    band('Needs attention', String(attention.length), 'red');
    for (const [id, r] of attention) row(id, r, now, true);
  }
  const rest = entries.filter(e => !attention.includes(e) && e[1].task);
  if (shownDay === today) {
    const overdue = rest.filter(([, r]) => !r.task!.completed && reminderAt(r) !== null && reminderAt(r)! < today);
    if (overdue.length) {
      const first = startOfDay(reminderAt(overdue[0][1])!);
      const same = overdue.every(([, r]) => startOfDay(reminderAt(r)!) === first);
      band(same ? `Overdue · ${shortDay(first)}` : 'Overdue', String(overdue.length), 'red');
      for (const [id, r] of overdue) row(id, r, now, !same);
    }
  }
  const next = addDays(shownDay, 1);
  const dayEntries = rest.filter(([, r]) => { const d = reminderAt(r); return d !== null && d >= shownDay && d < next; });
  // Markers sit at the top of their day as all-day rows.
  const markers = rest.filter(([, r]) => markerDay(r) === shownDay);
  // What Enter would add: a reminder at the typed time, otherwise a marker on the shown day.
  const typing = !!parsed.title;
  const adding = typing && (target === null || startOfDay(target) === shownDay);
  const total = markers.length + dayEntries.length;
  band(shownDay === today ? 'Today' : longDay(shownDay), adding ? `${total} + 1` : String(total));
  const newRow = (marker: boolean) => {
    const line = el('div', 'gt-row new');
    line.append(el('span', `tm red${marker ? ' ad' : ''}`, marker ? 'all day' : clock(target!)), el('span', `node r${marker ? ' mk' : ''}`),
      el('span', 't', parsed.title), el('span', 'n red', 'new'), el('span'));
    gutter.append(line);
  };
  if (adding && target === null) newRow(true);
  for (const [id, r] of markers) row(id, r, now);
  let drewNow = shownDay !== today;
  let drewNew = !adding || target === null;
  for (const [id, r] of dayEntries) {
    const due = reminderAt(r)!;
    if (!drewNew && target! <= due) { newRow(false); drewNew = true; }
    if (!drewNow && now <= due) { nowLine(now); drewNow = true; }
    row(id, r, now);
  }
  if (!drewNew) newRow(false);
  if (!drewNow) nowLine(now);
  if (!total && !adding && shownDay !== today) gutter.append(el('p', 'gt-empty lbl', 'Nothing planned'));

  // Reminders without a time, from before markers existed or saved from the sheet without one.
  const unscheduled = rest.filter(([, r]) => reminderAt(r) === null && markerDay(r) === null && !r.task!.completed);
  if (showUnscheduled && unscheduled.length) {
    band('Unscheduled', String(unscheduled.length));
    for (const [id, r] of unscheduled) row(id, r, now);
  }
  $('#unscheduled').textContent = unscheduled.length ? `${unscheduled.length} unscheduled ${showUnscheduled ? '‹' : '›'}` : '';
  quick.placeholder = `New task for ${shortDay(selectedDay)}`;
  renderHint(parsed, target);
}

function renderHint(parsed = parse(quick.value), target = quickTarget(parsed)) {
  const hint = $<HTMLElement>('#hint');
  if (hint.classList.contains('message')) return;
  hint.classList.toggle('red', target !== null);
  hint.textContent = !quick.value.trim() ? `↵ adds to ${shortDay(selectedDay)}`
    : target !== null ? `→ ${longDay(startOfDay(target))} · ${clock(target)}`
    : `↵ adds to ${longDay(parsed.day ?? selectedDay)} · all day`;
}

// ── Input bar ──
quick.addEventListener('input', () => render());
$<HTMLFormElement>('#quick').addEventListener('submit', event => {
  event.preventDefault();
  void run(async () => {
    const parsed = parse(quick.value);
    if (!parsed.title) { quick.focus(); return; }
    const remindAt = quickTarget(parsed);
    const day = remindAt !== null ? startOfDay(remindAt) : parsed.day ?? selectedDay;
    quick.value = '';
    // A time makes a reminder; without one it is a marker on that day.
    await save(null, remindAt !== null
      ? { schemaVersion: 1, title: parsed.title, notes: '', completed: false, remindAt }
      : { schemaVersion: 2, title: parsed.title, notes: '', completed: false, remindAt: null, kind: 'marker', date: isoDate(day) });
    selectedDay = day;
    render();
    await askForNotifications(remindAt);
  });
});
$('#unscheduled').addEventListener('click', () => { showUnscheduled = !showUnscheduled; render(); });

// ── Reschedule: press and hold a row, then drop it on a day in the strip. It keeps its time. ──
let lifted: string | null = null;
function holdToMove(line: HTMLElement, id: string) {
  let timer = 0, startX = 0, startY = 0;
  const cancel = () => clearTimeout(timer);
  line.addEventListener('pointerdown', event => {
    if (event.button !== 0 || (event.target as HTMLElement).closest('.node')) return;
    startX = event.clientX; startY = event.clientY;
    timer = window.setTimeout(() => {
      lifted = id; line.classList.add('lifted');
      document.body.classList.add('dragging');
      navigator.vibrate?.(15);
    }, 450);
  });
  line.addEventListener('pointermove', event => {
    if (!lifted && Math.hypot(event.clientX - startX, event.clientY - startY) > 10) cancel();
  });
  line.addEventListener('pointerup', cancel);
  line.addEventListener('pointercancel', cancel);
}
function dayUnder(x: number, y: number) {
  const cell = document.elementFromPoint(x, y)?.closest<HTMLElement>('.strip button');
  return cell?.dataset.day ? Number(cell.dataset.day) : null;
}
function markDrop(day: number | null) {
  for (const cell of document.querySelectorAll<HTMLElement>('.strip button')) cell.classList.toggle('drop', Number(cell.dataset.day) === day);
}
document.addEventListener('pointermove', event => { if (lifted) markDrop(dayUnder(event.clientX, event.clientY)); });
// Once a row is lifted, the finger drags it rather than scrolling the page.
document.addEventListener('touchmove', event => { if (lifted) event.preventDefault(); }, { passive: false });
document.addEventListener('pointerup', event => {
  if (!lifted) return;
  const id = lifted, day = dayUnder(event.clientX, event.clientY);
  // Let the click that follows this release see `lifted` and leave the row closed.
  setTimeout(() => { lifted = null; document.body.classList.remove('dragging'); markDrop(null); render(); });
  const task = data.records[id]?.task;
  if (day === null || !task) return;
  if (task.kind === 'marker') {
    if (task.date === isoDate(day)) return;
    void run(async () => { await save(id, { ...task, date: isoDate(day) }); selectedDay = day; status(`Moved to ${longDay(day)}`); });
    return;
  }
  if (task.remindAt === null) return;
  const from = new Date(task.remindAt), to = new Date(day);
  to.setHours(from.getHours(), from.getMinutes(), 0, 0);
  if (to.getTime() === task.remindAt) return;
  void run(async () => {
    await save(id, { ...task, remindAt: to.getTime() });
    selectedDay = day;
    status(`Moved to ${longDay(day)} · ${clock(to.getTime())}`);
  });
});

// ── Swipe the list sideways to change day ──
let listX: number | null = null, listY = 0;
gutter.addEventListener('touchstart', event => { listX = event.touches[0].clientX; listY = event.touches[0].clientY; }, { passive: true });
gutter.addEventListener('touchend', event => {
  if (listX === null || lifted) { listX = null; return; }
  const dx = event.changedTouches[0].clientX - listX, dy = event.changedTouches[0].clientY - listY;
  listX = null;
  if (Math.abs(dx) > 60 && Math.abs(dx) > 2 * Math.abs(dy)) { selectedDay = addDays(selectedDay, dx < 0 ? 1 : -1); expanded = null; render(); }
});

// ── Strip navigation: swipe on touch, arrow keys on desktop ──
let swipeX: number | null = null;
$<HTMLDivElement>('#strip').addEventListener('touchstart', event => { swipeX = event.touches[0].clientX; }, { passive: true });
$<HTMLDivElement>('#strip').addEventListener('touchend', event => {
  if (swipeX === null) return;
  const dx = event.changedTouches[0].clientX - swipeX;
  swipeX = null;
  if (Math.abs(dx) > 50) { selectedDay = addDays(selectedDay, dx < 0 ? 7 : -7); expanded = null; render(); }
});

// ── The sheet: every field, for adding with notes and for editing ──
function chooseDay(day: number) {
  field('date').value = isoDate(day);
  $('#readout-day').textContent = longDay(day);
  const chips = [...$<HTMLDivElement>('#day-chips').children] as HTMLElement[];
  for (const chip of chips) chip.classList.toggle('on', chip.dataset.day === String(day));
  (field('date') as HTMLInputElement).hidden = chips.some(c => c.dataset.day === String(day));
}
function renderChips() {
  const chips = $<HTMLDivElement>('#day-chips');
  chips.replaceChildren();
  const today = startOfDay(Date.now());
  for (const [day, label] of [[today, 'Today'], [addDays(today, 1), 'Tomorrow'], [addDays(today, 2), weekday(addDays(today, 2))]] as const) {
    const chip = el('button', 'pill', label);
    chip.type = 'button'; chip.dataset.day = String(day);
    chip.addEventListener('click', () => chooseDay(day));
    chips.append(chip);
  }
  const pick = el('button', 'pill', 'Pick');
  pick.type = 'button';
  pick.addEventListener('click', () => { const input = field('date') as HTMLInputElement; input.hidden = false; input.showPicker?.(); input.focus(); });
  chips.append(pick);
}
// The type keys: a reminder has a time, a marker only a day. Alarms wait for their ringing path.
let kind: 'reminder' | 'marker' = 'reminder';
function chooseKind(next: 'reminder' | 'marker') {
  kind = next;
  for (const key of document.querySelectorAll<HTMLElement>('.key[data-kind]')) {
    key.classList.toggle('on', key.dataset.kind === next);
    key.setAttribute('aria-pressed', String(key.dataset.kind === next));
  }
  ($('.readout input') as HTMLInputElement).hidden = next === 'marker';
}
for (const key of document.querySelectorAll<HTMLElement>('.key[data-kind]')) {
  key.addEventListener('click', () => chooseKind(key.dataset.kind as 'reminder' | 'marker'));
}
function openSheet(id: string | null) {
  const fromBar = id ? null : parse(quick.value);
  editing = id;
  form.reset();
  renderChips();
  const record = id ? data.records[id] : null;
  const task = record?.task ?? null;
  const at = task?.remindAt ?? (fromBar ? quickTarget(fromBar) : null);
  const marked = record ? markerDay(record) : null;
  chooseKind(task ? (task.kind === 'marker' ? 'marker' : 'reminder') : at !== null ? 'reminder' : 'marker');
  field('title').value = task?.title ?? fromBar?.title ?? '';
  field('notes').value = task?.notes ?? '';
  field('time').value = at !== null ? clock(at) : '';
  chooseDay(marked ?? (at !== null ? startOfDay(at) : fromBar?.day ?? Math.max(selectedDay, startOfDay(Date.now()))));
  $('#sheet-submit').textContent = id ? '↵ Save' : '↵ Add';
  sheet.hidden = false;
  field('title').focus();
}
function resetEditor() { editing = null; form.reset(); sheet.hidden = true; }

// Android's back gesture asks this first (MainActivity.kt): close what is open and return true,
// or return false to let back leave the app.
(window as unknown as { nothingToDoBack: () => boolean }).nothingToDoBack = () => {
  if (!settings.hidden) { closeSettings(); return true; }
  if (!sheet.hidden) { resetEditor(); return true; }
  if (quick.value) { quick.value = ''; quick.blur(); render(); return true; }
  if (expanded) { expanded = null; render(); return true; }
  return false;
};
$('#open-sheet').addEventListener('click', () => openSheet(null));
field('date').addEventListener('change', () => {
  const value = field('date').value;
  if (value) chooseDay(startOfDay(new Date(`${value}T00:00`).getTime()));
});
sheet.addEventListener('click', event => { if (event.target === sheet) resetEditor(); });
cancelEdit.addEventListener('click', resetEditor);
form.addEventListener('submit', event => {
  event.preventDefault();
  void run(async () => {
    const time = field('time').value, date = field('date').value;
    const marker = kind === 'marker';
    const remindAt = !marker && time ? new Date(`${date}T${time}`).getTime() : null;
    if (remindAt !== null && !Number.isFinite(remindAt)) throw new Error('Enter a valid date and time.');
    const existing = editing ? data.records[editing]?.task : null;
    const wasNew = !editing;
    const { kind: _kind, date: _date, ...rest } = existing ?? {};
    await save(editing, {
      ...rest, schemaVersion: marker ? 2 : 1, title: field('title').value, notes: field('notes').value,
      completed: existing?.completed ?? false, remindAt,
      ...(marker ? { kind: 'marker' as const, date } : {}),
    } satisfies Task);
    if (remindAt !== null) selectedDay = startOfDay(remindAt);
    else if (marker) selectedDay = new Date(`${date}T00:00`).getTime();
    if (wasNew) quick.value = '';
    resetEditor(); render();
    await askForNotifications(remindAt);
  });
});
// Android schedules reminders with the OS; desktop shows them from the running app.
$('#delivery-hint').textContent = /Android/i.test(navigator.userAgent)
  ? 'Times use this device’s timezone. Reminders fire even when the app is closed.'
  : 'Times use this device’s timezone. Reminders need the app running; closing the window keeps it in the tray.';

// ── Notifications ──
async function checkNotifications() {
  const off = !(await isPermissionGranted());
  if (off !== notificationsOff) { notificationsOff = off; render(); }
}
async function enableNotifications() {
  if (await isPermissionGranted() || await requestPermission() === 'granted') { notificationsOff = false; render(); return; }
  // After a refusal Android stops showing its prompt; only the app's settings page can help.
  try { await invoke('open_notification_settings'); }
  catch { status('Notifications are blocked. Turn them on in system settings.'); }
}
// Ask when it first matters; without permission Android drops the reminder silently.
async function askForNotifications(remindAt: number | null) {
  if (remindAt !== null && !(await isPermissionGranted())) {
    notificationsOff = await requestPermission() !== 'granted';
    render();
  }
}

// ── Settings: sync and backup ──
function showSyncStatus(state: SyncStatus) {
  const changed = sync.connected !== state.connected || !!sync.error !== !!state.error;
  sync = state;
  if (changed) render();
  syncForm.hidden = state.connected;
  $<HTMLElement>('#sync-connected').hidden = !state.connected;
  const message = !state.connected ? 'Not connected. Tasks are stored on this device only.'
    : state.catchingUp ? 'Downloading sync history…'
    : state.syncing ? 'Syncing…'
    : state.error ? `${state.error} Retrying in ${state.retryInSeconds ?? 300}s.`
    : state.lastSyncedAt !== null ? `Last synced ${new Date(state.lastSyncedAt).toLocaleTimeString()}.`
    : 'Connected.';
  const remembered = state.connected ? (state.remembered ? ' Reconnects automatically after restarting.' : ' Connected for this session only.') : '';
  $('#sync-connection').textContent = message + remembered;
  $('#sync-notice').textContent = state.notice ?? '';
}
async function wakeSync() {
  if (sync.connected) await invoke('sync_now');
}
// Focus can fire many times a minute (with focus-follows-mouse, on every pass of the pointer), so
// returning to the app syncs at most every 15 seconds. The worker's 30-second poll covers the rest.
let lastReturnSync = 0;
function syncOnReturn() {
  const recent = Math.max(lastReturnSync, sync.lastSyncedAt ?? 0);
  if (sync.syncing || Date.now() - recent < 15_000) return;
  lastReturnSync = Date.now();
  void wakeSync().catch(error => status(String(error)));
}
const closeSettings = () => { settings.hidden = true; };
const openSettings = () => { settings.hidden = false; };
$('#open-settings').addEventListener('click', openSettings);
$('#close-settings').addEventListener('click', closeSettings);
settings.addEventListener('click', event => { if (event.target === settings) closeSettings(); });
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
$('#sync').addEventListener('click', () => { void run(async () => { await invoke('sync_now'); status('Sync requested.'); }); });
$('#disconnect').addEventListener('click', () => { void run(async () => {
  await invoke('disconnect_sync');
  status('Sync disconnected. Tasks stay on this device.');
}); });

// Local time for backup file names, e.g. 2026-10-29-0915.
function stamp() {
  const d = new Date();
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}-${two(d.getHours())}${two(d.getMinutes())}`;
}
interface BackupSummary { tasks: number; skipped: number; exportedAt: number }
const backupFile = $<HTMLInputElement>('#backup-file');
const backupChoice = $<HTMLDivElement>('#backup-choice');
let chosenBackup: string | null = null;
function closeBackupChoice() { chosenBackup = null; backupFile.value = ''; backupChoice.hidden = true; }
$('#export-backup').addEventListener('click', () => { void run(async () => {
  status(`Backup saved to ${await invoke<string>('export_backup', { stamp: stamp() })}`);
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
  status(`Added ${done.tasks} task(s)${done.skipped ? `; left out ${done.skipped}` : ''}`);
}); });
$('#backup-restore').addEventListener('click', () => { void run(async () => {
  if (!chosenBackup) return;
  const done = await invoke<BackupSummary & { previous: string }>('restore_backup', { json: chosenBackup, stamp: stamp() });
  closeBackupChoice(); await refresh();
  status(`Restored ${done.tasks} task(s); the replaced data is in ${done.previous}`);
}); });

// ── Keyboard: n types a new task, arrows change day, Escape closes ──
document.addEventListener('keydown', event => {
  const typing = event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement;
  if (event.key === 'Escape') {
    if (!settings.hidden) closeSettings();
    else if (!sheet.hidden) resetEditor();
    else if (document.activeElement === quick) { quick.value = ''; quick.blur(); render(); }
    return;
  }
  if (typing || event.ctrlKey || event.metaKey || event.altKey || !sheet.hidden || !settings.hidden) return;
  if (event.key === 'n') { event.preventDefault(); quick.focus(); }
  if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
    selectedDay = addDays(selectedDay, event.key === 'ArrowLeft' ? -1 : 1); expanded = null; render();
  }
});
// Keep the NOW line and countdowns current.
setInterval(() => { if (!busy && sheet.hidden && !quick.value) render(); }, 30_000);

async function start() {
  if (!isTauri()) {
    status('Open the native app with npm run tauri dev; the browser preview stores nothing.');
    render();
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
    render();
  });
  await refresh();
  // First run with nothing here yet: show where sync is set up.
  if (!sync.connected && !sync.remembered && Object.keys(data.records).length === 0) openSettings();
  await wakeSync();
}
void start().catch(error => status(String(error)));
