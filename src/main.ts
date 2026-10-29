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
type Repeat = { type: 'fixed'; minutes: number } | { type: 'calendar'; unit: 'day' | 'week' | 'month' | 'year'; every: number; weekdays?: number[] };
interface Task {
  schemaVersion: number; title: string; notes: string; completed: boolean; remindAt: number | null;
  kind?: 'reminder' | 'marker' | 'alarm'; date?: string; durationMinutes?: number; repeat?: Repeat;
  repeatFrom?: number; timeZone?: string; warnings?: number[];
}
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
      <div class="top-right">
        <button id="period" class="lbl" type="button" aria-label="Month view"></button>
        <button id="open-settings" class="dots" type="button" aria-label="Settings"><i></i><i></i><i></i></button>
      </div>
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
      <form id="task-form" class="panel add" novalidate>
        <button type="button" id="sheet-handle" class="handle" aria-label="Close"></button>
        <span id="sheet-label" class="lbl">New entry</span>
        <input id="sheet-title" name="title" class="input" required maxlength="500" autocomplete="off" placeholder="What needs doing?" aria-label="Title">
        <div id="keys" class="keys" role="radiogroup" aria-label="Type"></div>
        <button type="button" id="readout" class="readout" aria-label="Time" aria-expanded="false"></button>
        <div id="day-chips" class="chips" role="radiogroup" aria-label="Day"></div>
        <div id="when-edit"></div>
        <div id="tiles" class="tiles"></div>
        <p id="delivery-hint" class="note"></p>
        <button class="btn f" type="submit"><span class="rdot"></span><span id="sheet-submit">Add reminder</span></button>
        <input type="date" id="pick-date" class="pick" tabindex="-1" aria-hidden="true">
      </form>
    </div>
    <div id="month" class="sheet" role="dialog" aria-modal="true" aria-labelledby="month-title" hidden>
      <div class="panel month-panel">
        <div class="month-head">
          <button id="month-prev" class="icon" type="button" aria-label="Previous month">‹</button>
          <h2 id="month-title" class="dot"></h2>
          <button id="month-next" class="icon" type="button" aria-label="Next month">›</button>
        </div>
        <div class="month-days lbl"><span>M</span><span>T</span><span>W</span><span>T</span><span>F</span><span>S</span><span>S</span></div>
        <div id="month-grid" class="month-grid"></div>
      </div>
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
// The first day the timeline lists; today unless an earlier day was picked.
let listStart = startOfDay(Date.now());
let scrollTo: number | null = null;
let expanded: string | null = null;
let showUnscheduled = false;
type Entry = [string, TaskRecord];

// ── Typing in the bar: "Call the bank fri 8am" ──
const DAYS = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];
const FULL_DAYS = ['sunday', 'monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday'];
interface Parsed {
  title: string; day: number | null; minutes: number | null;
  alarm: boolean; repeat: Repeat | null; warnings: number[]; duration: number | null;
}
// "45m", "2h", "1h30", "3d", "1w", "6mo" as minutes; null if it is not a duration.
function minutesOf(word: string): number | null {
  const mixed = /^(\d+)h(\d{1,2})$/.exec(word);
  if (mixed) return Number(mixed[1]) * 60 + Number(mixed[2]);
  const m = /^(\d+)\s*(m|min|mins|h|hr|hrs|hour|hours|d|day|days|w|wk|wks|week|weeks)$/.exec(word);
  if (!m) return null;
  const n = Number(m[1]), unit = m[2][0];
  return n * ({ m: 1, h: 60, d: 1440, w: 10080 } as Record<string, number>)[unit];
}
const UNITS: Record<string, 'day' | 'week' | 'month' | 'year'> = {
  day: 'day', days: 'day', d: 'day', week: 'week', weeks: 'week', wk: 'week', wks: 'week', w: 'week',
  month: 'month', months: 'month', mo: 'month', year: 'year', years: 'year', y: 'year',
};
const weekdayOf = (w: string) => { const i = Math.max(DAYS.indexOf(w), FULL_DAYS.indexOf(w)); return i < 0 ? null : i === 0 ? 7 : i; };

// Typing in the bar fills the same fields as the sheet: "Gym weekdays 7am warn 10m",
// "Call the bank fri 8am", "Bins weekly", "Dentist tue 9am for 45m", "Wake up 6:30 alarm".
function parse(text: string): Parsed {
  let day: number | null = null, minutes: number | null = null, repeat: Repeat | null = null, duration: number | null = null;
  let alarmWord: string | null = null;
  const warnings: number[] = [];
  const today = startOfDay(Date.now());
  const kept: string[] = [];
  const words = text.trim().split(/\s+/).filter(Boolean);
  for (let i = 0; i < words.length; i++) {
    const word = words[i], w = word.toLowerCase();
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
    if (repeat === null) {
      const named: Record<string, Repeat> = {
        daily: { type: 'calendar', unit: 'day', every: 1 }, everyday: { type: 'calendar', unit: 'day', every: 1 },
        weekdays: { type: 'calendar', unit: 'day', every: 1, weekdays: [1, 2, 3, 4, 5] },
        weekends: { type: 'calendar', unit: 'day', every: 1, weekdays: [6, 7] },
        weekly: { type: 'calendar', unit: 'week', every: 1 }, fortnightly: { type: 'calendar', unit: 'week', every: 2 },
        monthly: { type: 'calendar', unit: 'month', every: 1 }, yearly: { type: 'calendar', unit: 'year', every: 1 },
        annually: { type: 'calendar', unit: 'year', every: 1 },
      };
      if (named[w]) { repeat = named[w]; continue; }
      if (w === 'every' && i + 1 < words.length) {
        const next = words[i + 1].toLowerCase();
        // "every mon wed": weekly on those days.
        const set: number[] = [];
        let j = i + 1;
        while (j < words.length && weekdayOf(words[j].toLowerCase()) !== null) set.push(weekdayOf(words[j++].toLowerCase())!);
        if (set.length) { repeat = { type: 'calendar', unit: 'week', every: 1, weekdays: [...new Set(set)].sort() }; i = j - 1; continue; }
        if (next === 'day') { repeat = { type: 'calendar', unit: 'day', every: 1 }; i++; continue; }
        // "every 3 days", "every 2 weeks", "every 8h", "every 28d"
        const count = /^(\d+)([a-z]*)$/.exec(next);
        if (count) {
          const n = Number(count[1]);
          const unitWord = count[2] || (words[i + 2] ?? '').toLowerCase();
          const consumed = count[2] ? 1 : 2;
          if (unitWord === 'h' || unitWord === 'hours' || unitWord === 'hrs') { repeat = { type: 'fixed', minutes: n * 60 }; i += consumed; continue; }
          if (UNITS[unitWord] && n > 0) { repeat = { type: 'calendar', unit: UNITS[unitWord], every: n }; i += consumed; continue; }
        }
      }
    }
    if (w === 'warn') {
      let j = i + 1;
      while (j < words.length && minutesOf(words[j].toLowerCase()) !== null) warnings.push(minutesOf(words[j++].toLowerCase())!);
      if (j > i + 1) { i = j - 1; continue; }
    }
    if (w === 'for' && duration === null && i + 1 < words.length && minutesOf(words[i + 1].toLowerCase()) !== null) {
      duration = minutesOf(words[i + 1].toLowerCase()); i++; continue;
    }
    if (w === '!!' || (w === 'alarm' && kept.length)) { alarmWord = word; continue; }
    const index = weekdayOf(w);
    if (day === null && index !== null) { day = addDays(today, ((index % 7) - new Date(today).getDay() + 7) % 7); continue; }
    if (w === 'at' && kept.length) continue;
    kept.push(word);
  }
  // "alarm" only means the type when there is a time to ring at; otherwise it is a word.
  const alarm = alarmWord !== null && (minutes !== null || alarmWord === '!!');
  if (alarmWord !== null && !alarm) kept.push(alarmWord);
  // A weekday repeat without a day starts on the next day in its set.
  if (day === null && repeat?.type === 'calendar' && repeat.weekdays?.length) {
    for (let k = 0; k < 7; k++) {
      const d = addDays(today, k), wd = ((new Date(d).getDay() + 6) % 7) + 1;
      const passed = k === 0 && minutes !== null && minutes <= new Date().getHours() * 60 + new Date().getMinutes();
      if (repeat.weekdays.includes(wd) && !passed) { day = d; break; }
    }
  }
  return { title: kept.join(' '), day, minutes, alarm, repeat, warnings: [...new Set(warnings)], duration };
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
  // With no day typed, a time already past today means tomorrow.
  if (parsed.day === null && d.getTime() <= Date.now() && startOfDay(d.getTime()) === startOfDay(Date.now())) d.setDate(d.getDate() + 1);
  return d.getTime();
}

// ── Rendering ──
// Open entries on a day, for the strip and month view's dot loads: red dots are overdue.
function loadOn(day: number, now: number) {
  const next = addDays(day, 1);
  const open = Object.values(data.records).filter(r => r.task && !r.task.completed && !r.unsupported);
  const dues = open.map(reminderAt).filter((d): d is number => d !== null && d >= day && d < next);
  const marks = open.filter(r => markerDay(r) === day).length;
  return [...Array(marks).fill(''), ...dues.map(due => (due <= now ? 'r' : ''))] as string[];
}
function dayCell(day: number, shownDay: number, now: number, month: number, letter: boolean) {
  const cell = el('button', day === shownDay ? 'on' : '');
  cell.type = 'button';
  const dots = loadOn(day, now);
  cell.setAttribute('aria-label', `${new Date(day).toDateString()}, ${dots.length} entr${dots.length === 1 ? 'y' : 'ies'}`);
  cell.setAttribute('aria-pressed', String(day === shownDay));
  cell.classList.toggle('today', day === startOfDay(now));
  const load = el('span', 'load');
  for (const dot of dots.slice(0, 4)) load.append(el('i', dot));
  if (letter) cell.append(el('span', 'lbl', new Date(day).toLocaleDateString([], { weekday: 'narrow' })));
  cell.append(el('b', new Date(day).getMonth() !== month ? 'dim' : '', two(new Date(day).getDate())), load);
  cell.dataset.day = String(day);
  return cell;
}

// The strip shows six days from today (or from wherever the timeline has moved to); its seventh
// box always opens the month view.
let stripStart = startOfDay(Date.now());
function stripFor(day: number) {
  if (day >= stripStart && day < addDays(stripStart, 6)) return stripStart;
  const today = startOfDay(Date.now());
  return day >= today && day < addDays(today, 6) ? today : day;
}

function renderHeader(shownDay: number, preview: boolean) {
  $('#date-heading').textContent = shortDay(shownDay);
  const label = $<HTMLButtonElement>('#period');
  label.classList.toggle('red', preview || !!sync.error);
  label.textContent = preview ? 'preview' : sync.error ? 'sync error'
    : `${new Date(shownDay).toLocaleDateString([], { month: 'short', year: 'numeric' })} · wk ${isoWeek(shownDay)}`;
  $('#open-settings').classList.toggle('red', !!sync.error);
  const strip = $<HTMLDivElement>('#strip');
  strip.replaceChildren();
  const now = Date.now();
  const start = preview ? stripFor(shownDay) : (stripStart = stripFor(shownDay));
  const month = new Date(shownDay).getMonth();
  for (let i = 0; i < 6; i++) {
    const day = addDays(start, i);
    const cell = dayCell(day, shownDay, now, month, true);
    cell.addEventListener('click', () => goToDay(day));
    strip.append(cell);
  }
  const more = el('button', 'month-cell');
  more.type = 'button';
  more.setAttribute('aria-label', 'Month view');
  more.append(el('span', 'lbl', 'Month'), el('b', '', new Date(shownDay).toLocaleDateString([], { month: 'short' })), el('span', 'grid-icon'));
  more.addEventListener('click', () => openMonth(shownDay));
  strip.append(more);
}

// ── Month view: the same day cells and dot loads; tapping a day jumps the timeline there ──
const monthSheet = $<HTMLDivElement>('#month');
let monthShown = startOfDay(Date.now());
function renderMonth() {
  const first = new Date(monthShown); first.setDate(1);
  const start = weekStart(first.getTime());
  const month = first.getMonth();
  $('#month-title').textContent = first.toLocaleDateString([], { month: 'short', year: 'numeric' });
  const grid = $<HTMLDivElement>('#month-grid');
  grid.replaceChildren();
  const now = Date.now();
  for (let i = 0; i < 42; i++) {
    const day = addDays(start, i);
    if (i >= 35 && new Date(day).getMonth() !== month) break;
    const cell = dayCell(day, selectedDay, now, month, false);
    cell.addEventListener('click', () => { monthSheet.hidden = true; goToDay(day); });
    grid.append(cell);
  }
}
function openMonth(day: number) { monthShown = day; renderMonth(); monthSheet.hidden = false; }
function stepMonth(by: number) { const d = new Date(monthShown); d.setDate(1); d.setMonth(d.getMonth() + by); monthShown = d.getTime(); renderMonth(); }
$('#month-prev').addEventListener('click', () => stepMonth(-1));
$('#month-next').addEventListener('click', () => stepMonth(1));
monthSheet.addEventListener('click', event => { if (event.target === monthSheet) monthSheet.hidden = true; });
let monthX: number | null = null;
monthSheet.addEventListener('touchstart', event => { monthX = event.touches[0].clientX; }, { passive: true });
monthSheet.addEventListener('touchend', event => {
  if (monthX === null) return;
  const dx = event.changedTouches[0].clientX - monthX;
  monthX = null;
  if (Math.abs(dx) > 60) stepMonth(dx < 0 ? 1 : -1);
});
$('#period').addEventListener('click', () => { if (sync.error) openSettings(); else openMonth(selectedDay); });

// ── A desktop alarm ringing now (Android shows its own full-screen alarm) ──
interface Ringing { taskId: string; title: string; at: number }
let ringing: Ringing | null = null;
function ringingBlock(r: Ringing) {
  const box = el('div', 'ringing');
  box.append(el('span', 'lbl', `Alarm · ${clock(r.at)}`), el('p', 'ring-title', r.title));
  const actions = el('div', 'row-actions');
  // Stopping takes a press-and-hold, as in the design, so a stray tap cannot silence it.
  const stop = el('button', 'pill on', 'Hold to stop');
  stop.type = 'button';
  let hold = 0;
  stop.addEventListener('pointerdown', () => {
    stop.textContent = 'Hold…';
    hold = window.setTimeout(() => { void run(() => invoke('answer_alarm', { snooze: false })); }, 900);
  });
  for (const end of ['pointerup', 'pointerleave', 'pointercancel']) stop.addEventListener(end, () => { clearTimeout(hold); stop.textContent = 'Hold to stop'; });
  actions.append(stop, pill('Snooze 10m', () => invoke('answer_alarm', { snooze: true })));
  box.append(actions);
  return box;
}

function band(label: string, count: string, className = '', onClick?: () => void, day?: number) {
  const b = el(onClick ? 'button' : 'div', `gt-band ${className}`);
  if (day !== undefined) b.dataset.day = String(day);
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
  const alarm = task?.kind === 'alarm';
  const done = !!task?.completed;
  const late = !done && due !== null && due <= now;
  const line = el('div', 'gt-row');
  line.classList.toggle('open', expanded === id);
  // The node on the rail is the checkbox: a hollow ring, a square for markers, a filled double
  // ring for alarms.
  const node = el('button', `node${marker ? ' mk' : alarm ? ' al' : ''}${done ? ' x' : ''}${late ? ' r' : ''}`);
  node.type = 'button';
  node.disabled = readOnly || !task;
  node.setAttribute('aria-label', done ? `Reopen ${task?.title}` : `Complete ${task?.title ?? 'task'}`);
  if (task) node.addEventListener('click', event => {
    event.stopPropagation();
    void run(async () => {
      await save(id, { ...task, completed: !done });
      // A repeating task moves on rather than completing; say where it went.
      const next = data.records[id]?.task;
      if (task.repeat && !done && next && !next.completed) {
        status(`Next · ${next.kind === 'marker' && next.date ? shortDay(new Date(`${next.date}T00:00`).getTime()) : next.remindAt !== null ? `${shortDay(next.remindAt)} · ${clock(next.remindAt)}` : ''}`);
      }
    });
  });
  const markedDay = markerDay(record);
  const time = marker && markedDay !== null ? (showDay ? shortDay(markedDay) : 'all day')
    : due === null ? '—' : clock(due);
  const title = el('span', `t${done ? ' done' : ''}`, task?.title ?? (readOnly ? 'From a newer app version' : 'Deleted on this device'));
  if (alarm && !done) title.append(el('span', 'tag', 'rings'));
  // Sync glyphs: synced is silent, pending a ring, conflict a red dot.
  const sy = el('span', record.conflict ? 'sy c' : record.dirty && sync.connected ? 'sy p' : 'sy');
  const tm = el('span', `tm${marker ? ' ad' : ''}${late ? ' red' : done ? ' dim' : ''}`);
  // Rows outside their own day band say which day: a small day label above the time.
  if (showDay && !marker && due !== null) tm.append(el('span', 'd', shortDay(due)));
  tm.append(document.createTextNode(time));
  line.append(tm, node, title,
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
  const facts = [
    task?.durationMinutes ? durationText(task.durationMinutes).join(' ').trim() : '',
    task?.repeat ? `repeats ${repeatText(task.repeat).join(' ').trim().toLowerCase()}` : '',
    task?.warnings?.length ? `warns ${[...task.warnings].sort((a, b) => b - a).map(w => spanText(w).toLowerCase()).join(', ')}` : '',
  ].filter(Boolean);
  if (facts.length) box.append(el('p', 'lbl', facts.join(' · ')));
  if (task && due !== null && due !== task.remindAt && !task.completed) box.append(el('p', 'lbl', `Snoozed on this device until ${clock(due)}`));
  if (record.dirty && sync.connected) box.append(el('p', 'lbl', 'Not yet synced'));
  if (readOnly) box.append(el('p', 'lbl', 'Changed by a newer app version. Update to edit.'));
  if (task && !readOnly) {
    const actions = el('div', 'row-actions');
    actions.append(pill('Edit', () => openSheet(id)));
    // Only a reminder that has gone off can be snoozed; snoozing earlier would bring it forward.
    if (!task.completed && due !== null && due <= now) actions.append(pill('Snooze 10m', async () => { await invoke('snooze_task', { id, minutes: 10 }); await refresh(); }));
    if (task.repeat) {
      const { repeat: _r, repeatFrom: _f, ...single } = task;
      actions.append(pill('End repeat', () => save(id, single)));
    }
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

  const keep = gutter.scrollTop;
  gutter.replaceChildren();
  const now = Date.now();
  const today = startOfDay(now);
  const entries = (Object.entries(data.records) as Entry[])
    .sort((a, b) => (reminderAt(a[1]) ?? Infinity) - (reminderAt(b[1]) ?? Infinity) || (a[1].task?.title ?? '').localeCompare(b[1].task?.title ?? ''));
  if (notificationsOff) band('Notifications off · turn on ›', '', 'red', () => { void run(enableNotifications); });
  if (ringing) gutter.append(ringingBlock(ringing));

  // Records needing a decision always show, whatever the day.
  const attention = entries.filter(([, r]) => r.conflict || (r.unsupported && !r.task));
  if (attention.length) {
    band('Needs attention', String(attention.length), 'red');
    for (const [id, r] of attention) row(id, r, now, true);
  }
  const rest = entries.filter(e => !attention.includes(e) && e[1].task);
  if (Math.min(listStart, shownDay) >= today) {
    const overdue = rest.filter(([, r]) => !r.task!.completed && reminderAt(r) !== null && reminderAt(r)! < today);
    if (overdue.length) {
      const first = startOfDay(reminderAt(overdue[0][1])!);
      const same = overdue.every(([, r]) => startOfDay(reminderAt(r)!) === first);
      band(same ? `Overdue · ${shortDay(first)}` : 'Overdue', String(overdue.length), 'red');
      for (const [id, r] of overdue) row(id, r, now, !same);
    }
  }
  // One continuous timeline from the first day shown: a band per day for the next two weeks,
  // then a band per month. Days with nothing show only when picked in the strip, or today.
  const first = Math.min(listStart, shownDay);
  const typing = !!parsed.title;
  const newDay = typing ? (target !== null ? startOfDay(target) : shownDay) : null;
  const dayOf = (r: TaskRecord) => markerDay(r) ?? (reminderAt(r) !== null ? startOfDay(reminderAt(r)!) : null);
  const byDay = new Map<number, Entry[]>();
  for (const entry of rest) {
    const day = dayOf(entry[1]);
    if (day === null || day < first) continue;
    byDay.set(day, [...(byDay.get(day) ?? []), entry]);
  }
  for (const day of [shownDay, today, newDay]) if (day !== null && day >= first && !byDay.has(day)) byDay.set(day, []);
  const horizon = addDays(first, 14);
  const newRow = (marker: boolean) => {
    const line = el('div', 'gt-row new');
    line.append(el('span', `tm red${marker ? ' ad' : ''}`, marker ? 'all day' : clock(target!)), el('span', `node r${marker ? ' mk' : ''}`),
      el('span', 't', parsed.title), el('span', 'n red', 'new'), el('span'));
    gutter.append(line);
  };
  // Markers first, then timed rows in order.
  const ordered = (list: Entry[]) => [...list].sort((a, b) =>
    Number(markerDay(b[1]) !== null) - Number(markerDay(a[1]) !== null) || (reminderAt(a[1]) ?? 0) - (reminderAt(b[1]) ?? 0));
  let month: number | null = null;
  for (const day of [...byDay.keys()].sort((a, b) => a - b)) {
    const list = ordered(byDay.get(day)!);
    if (day >= horizon) {
      const d = new Date(day), key = new Date(d.getFullYear(), d.getMonth(), 1).getTime();
      if (key !== month) {
        month = key;
        const inMonth = [...byDay.entries()].filter(([k]) => k >= horizon && new Date(k).getMonth() === d.getMonth() && new Date(k).getFullYear() === d.getFullYear());
        const label = d.toLocaleDateString([], { month: 'long', ...(d.getFullYear() !== new Date(today).getFullYear() ? { year: 'numeric' } : {}) });
        band(label, String(inMonth.reduce((n, [, l]) => n + l.length, 0)), '', undefined, day);
      }
      if (newDay === day) newRow(target === null);
      for (const [id, r] of list) row(id, r, now, true);
      continue;
    }
    const adding = newDay === day;
    band(day === today ? 'Today' : day === addDays(today, 1) ? `Tomorrow · ${longDay(day)}` : longDay(day),
      adding ? `${list.length} + 1` : String(list.length), '', undefined, day);
    if (adding && target === null) newRow(true);
    let drewNow = day !== today;
    let drewNew = !adding || target === null;
    for (const [id, r] of list) {
      const due = markerDay(r) === null ? reminderAt(r) : null;
      if (due !== null && !drewNew && target! <= due) { newRow(false); drewNew = true; }
      if (due !== null && !drewNow && now <= due) { nowLine(now); drewNow = true; }
      row(id, r, now);
    }
    if (!drewNew) newRow(false);
    if (!drewNow) nowLine(now);
    if (!list.length && !adding && day !== today) gutter.append(el('p', 'gt-empty lbl', 'Nothing planned'));
  }

  // Reminders without a time, from before markers existed or saved from the sheet without one.
  const unscheduled = rest.filter(([, r]) => reminderAt(r) === null && markerDay(r) === null && !r.task!.completed);
  if (showUnscheduled && unscheduled.length) {
    band('Unscheduled', String(unscheduled.length));
    for (const [id, r] of unscheduled) row(id, r, now);
  }
  $('#unscheduled').textContent = unscheduled.length ? `${unscheduled.length} unscheduled ${showUnscheduled ? '‹' : '›'}` : '';
  quick.placeholder = `New task for ${shortDay(selectedDay)}`;
  renderHint(parsed, target);
  // Room after the last band, so any day can be scrolled to the top of the timeline.
  const spacer = el('div', 'gt-end');
  gutter.append(spacer);
  const lastBand = [...gutter.querySelectorAll<HTMLElement>('.gt-band')].pop();
  spacer.style.height = `${Math.max(0, gutter.clientHeight - (lastBand ? gutter.scrollHeight - lastBand.offsetTop : 0))}px`;
  const goal = previewDay ?? scrollTo;
  scrollTo = null;
  const goalBand = goal !== null ? gutter.querySelector<HTMLElement>(`.gt-band[data-day="${goal}"]`) : null;
  jumping = true;
  gutter.scrollTop = goalBand ? goalBand.offsetTop : keep;
  requestAnimationFrame(() => { jumping = false; });
}
// Set while the timeline scrolls itself, so the header keeps the day that was picked.
let jumping = false;

// The day shown in the header follows the band at the top of the timeline as it scrolls.
gutter.addEventListener('scroll', () => {
  if (jumping) return;
  let top: number | null = null;
  for (const b of gutter.querySelectorAll<HTMLElement>('.gt-band[data-day]')) {
    // The timeline is the bands' offset parent, so offsetTop is already measured within it.
    if (b.offsetTop <= gutter.scrollTop + 4) top = Number(b.dataset.day);
    else break;
  }
  if (top !== null && top !== selectedDay && !quick.value.trim()) {
    selectedDay = top;
    renderHeader(top, false);
    quick.placeholder = `New task for ${shortDay(selectedDay)}`;
    renderHint();
  }
}, { passive: true });

// Picking a day scrolls the timeline to it; days before the start extend it backwards.
function goToDay(day: number) {
  selectedDay = day;
  if (day < listStart) listStart = day;
  else if (day >= startOfDay(Date.now())) listStart = Math.min(listStart, startOfDay(Date.now()));
  expanded = null;
  scrollTo = day;
  render();
}

function renderHint(parsed = parse(quick.value), target = quickTarget(parsed)) {
  const hint = $<HTMLElement>('#hint');
  if (hint.classList.contains('message')) return;
  hint.classList.toggle('red', target !== null);
  const extras = [
    parsed.alarm ? 'alarm' : '',
    parsed.repeat ? repeatText(parsed.repeat).join(' ').trim().toLowerCase() : '',
    parsed.warnings.length ? `warn ${[...parsed.warnings].sort((a, b) => b - a).map(w => spanText(w).toLowerCase()).join(' ')}` : '',
    parsed.duration ? durationText(parsed.duration).join(' ').trim() : '',
  ].filter(Boolean).map(x => ` · ${x}`).join('');
  hint.textContent = !quick.value.trim() ? `↵ adds to ${shortDay(selectedDay)}`
    : target !== null ? `→ ${longDay(startOfDay(target))} · ${clock(target)}${extras}`
    : `↵ adds to ${longDay(parsed.day ?? selectedDay)} · all day${extras}`;
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
    // A time makes a reminder (or an alarm); without one it is a marker on that day.
    await save(null, {
      schemaVersion: 1, title: parsed.title, notes: '', completed: false, remindAt,
      ...(remindAt === null ? { kind: 'marker' as const, date: isoDate(day) } : parsed.alarm ? { kind: 'alarm' as const } : {}),
      ...(parsed.repeat ? { repeat: parsed.repeat } : {}),
      ...(parsed.warnings.length ? { warnings: parsed.warnings } : {}),
      ...(parsed.duration ? { durationMinutes: parsed.duration } : {}),
      ...(parsed.repeat || parsed.warnings.length ? { timeZone: zone } : {}),
    });
    goToDay(day);
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
    void run(async () => { await save(id, { ...task, date: isoDate(day) }); goToDay(day); status(`Moved to ${longDay(day)}`); });
    return;
  }
  if (task.remindAt === null) return;
  const from = new Date(task.remindAt), to = new Date(day);
  to.setHours(from.getHours(), from.getMinutes(), 0, 0);
  if (to.getTime() === task.remindAt) return;
  void run(async () => {
    await save(id, { ...task, remindAt: to.getTime() });
    goToDay(day);
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
  if (Math.abs(dx) > 60 && Math.abs(dx) > 2 * Math.abs(dy)) goToDay(addDays(selectedDay, dx < 0 ? 1 : -1));
});

// ── Strip navigation: swipe on touch, arrow keys on desktop ──
let swipeX: number | null = null;
$<HTMLDivElement>('#strip').addEventListener('touchstart', event => { swipeX = event.touches[0].clientX; }, { passive: true });
$<HTMLDivElement>('#strip').addEventListener('touchend', event => {
  if (swipeX === null) return;
  const dx = event.changedTouches[0].clientX - swipeX;
  swipeX = null;
  // Six days at a time, matching the strip.
  if (Math.abs(dx) > 50) { stripStart = addDays(stripStart, dx < 0 ? 6 : -6); goToDay(stripStart); }
});

// ── The add sheet: one view (revised from the deck's B, 2026-10-29): the short sheet's top half, then
// compact tiles that open in place (B1) with the B2 instruments inside. ──
type Kind = 'reminder' | 'marker' | 'alarm';
interface Draft { kind: Kind; day: number; minutes: number | null; duration: number | null; repeat: Repeat | null; warnings: number[]; notes: string }
let draft: Draft = { kind: 'reminder', day: startOfDay(Date.now()), minutes: null, duration: null, repeat: null, warnings: [], notes: '' };
let openTile: string | null = null;
let preview: number[] = [];
const titleInput = $<HTMLInputElement>('#sheet-title');
const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;

const at = (day: number, minutes: number) => { const d = new Date(day); d.setHours(Math.floor(minutes / 60), minutes % 60, 0, 0); return d.getTime(); };
const draftTime = () => (draft.kind !== 'marker' && draft.minutes !== null ? at(draft.day, draft.minutes) : null);
function durationText(m: number) { return m < 60 ? [String(m), 'min'] : m % 60 ? [`${Math.floor(m / 60)}h${two(m % 60)}`, ''] : [String(m / 60), m === 60 ? 'hour' : 'hours']; }
function spanText(m: number) { return m === 0 ? 'Day of' : m % 10080 === 0 ? `${m / 10080}W` : m % 1440 === 0 ? `${m / 1440}D` : m % 60 === 0 ? `${m / 60}H` : `${m}M`; }
const WEEKDAYS = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];
function repeatText(r: Repeat | null): [string, string] {
  if (!r) return ['Never', ''];
  if (r.type === 'fixed') return r.minutes % 1440 === 0 ? [String(r.minutes / 1440), 'd'] : [String(r.minutes / 60), 'h'];
  const days = r.weekdays ?? [];
  if (days.length && days.length < 7) {
    if (days.join() === '1,2,3,4,5') return ['Mon–Fri', ''];
    if (days.join() === '6,7') return ['Weekends', ''];
    const names = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
    return [days.length <= 3 ? days.map(d => names[d - 1]).join(' ') : days.map(d => WEEKDAYS[d - 1]).join(''), r.unit === 'week' && r.every > 1 ? `every ${r.every} wk` : ''];
  }
  if (r.unit === 'day') return r.every === 1 ? ['Daily', ''] : [String(r.every), 'd'];
  if (r.unit === 'week') return r.every === 1 ? ['Weekly', ''] : [String(r.every), 'wk'];
  if (r.unit === 'month') return r.every === 12 ? ['Yearly', ''] : [String(r.every), 'mo'];
  return ['Yearly', ''];
}
const sameRepeat = (a: Repeat | null, b: Repeat | null) => JSON.stringify(a) === JSON.stringify(b);

// The task this draft would save, merged over the stored one so unknown fields survive.
function draftTask(): Task {
  const existing = editing ? data.records[editing]?.task : null;
  const { kind: _k, date: _d, durationMinutes: _m, repeat: _r, timeZone: _z, warnings: _w, ...rest } = existing ?? {} as Partial<Task>;
  const marker = draft.kind === 'marker';
  const timed = draftTime();
  return {
    ...rest, schemaVersion: 1, title: titleInput.value, notes: draft.notes,
    completed: existing?.completed ?? false, remindAt: marker ? null : timed,
    ...(marker ? { kind: 'marker' as const, date: isoDate(draft.day) } : {}),
    ...(draft.kind === 'alarm' ? { kind: 'alarm' as const } : {}),
    ...(draft.duration ? { durationMinutes: draft.duration } : {}),
    ...(draft.repeat ? { repeat: draft.repeat } : {}),
    ...(draft.warnings.length ? { warnings: [...draft.warnings].sort((a, b) => b - a) } : {}),
    ...(draft.repeat || draft.warnings.length ? { timeZone: existing?.timeZone ?? zone } : {}),
  };
}

let previewTimer = 0;
function refreshPreview() {
  clearTimeout(previewTimer);
  preview = [];
  if (!draft.repeat || (draft.kind !== 'marker' && draft.minutes === null) || !isTauri()) return;
  previewTimer = window.setTimeout(async () => {
    try { preview = await invoke<number[]>('preview_occurrences', { task: { ...draftTask(), title: titleInput.value || 'Preview', schemaVersion: 2 }, count: 3 }); }
    catch { preview = []; }
    renderSheet();
  }, 150);
}

function choice(label: string, on: boolean, action: () => void, className = '') {
  const b = el('button', `${className}${on ? ' on' : ''}`, label);
  b.type = 'button';
  b.setAttribute('aria-pressed', String(on));
  b.addEventListener('click', event => { event.stopPropagation(); action(); renderSheet(); refreshPreview(); });
  return b;
}
function svg(tag: string, attrs: Record<string, string | number>) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
  return node;
}

// B2 instrument for When: a 24-hour dot dial, midnight at the top and noon at the bottom, with
// the daytime dots (06–18) brighter so the layout reads at a glance. Drag or tap anywhere on it
// to set the time in 5-minute steps; dragging moves only the handle, and the sheet redraws when
// the finger lifts.
function dial() {
  const wrap = el('div', 'dial-wrap');
  const face = el('div', 'dial');
  const s = svg('svg', { viewBox: '0 0 210 210' });
  const c = 105, r = 88;
  for (let i = 0; i < 48; i++) {
    const a = (i / 48) * 2 * Math.PI - Math.PI / 2, quarter = i % 12 === 0, day = i >= 12 && i <= 36;
    s.append(svg('circle', { cx: c + r * Math.cos(a), cy: c + r * Math.sin(a), r: quarter ? 2.6 : day ? 1.9 : 1.5, fill: quarter ? 'var(--fg)' : day ? 'var(--dim)' : 'var(--faint)' }));
    if (quarter) {
      const t = svg('text', { x: c + (r - 21) * Math.cos(a), y: c + (r - 21) * Math.sin(a) + 3, 'text-anchor': 'middle', 'font-family': 'Space Mono', 'font-size': 8, fill: 'var(--dim)' });
      t.textContent = two(i / 2);
      s.append(t);
    }
  }
  for (const [y, label] of [[8, 'MIDNIGHT'], [207, 'NOON']] as const) {
    const t = svg('text', { x: c, y, 'text-anchor': 'middle', 'font-family': 'Space Mono', 'font-size': 7, 'letter-spacing': 1, fill: 'var(--dim)' });
    t.textContent = label;
    s.append(t);
  }
  const line = svg('line', { x1: c, y1: c, stroke: 'var(--fg)', 'stroke-width': 1.5, 'stroke-dasharray': '1 3', 'stroke-linecap': 'round' });
  const knob = svg('circle', { r: 7, fill: 'var(--fg)' });
  s.append(line, knob);
  const time = el('span', 'dot big');
  const when = el('span', 'lbl');
  const centre = el('div', 'c');
  centre.append(time, el('br'), when);
  const draw = () => {
    const on = draft.minutes !== null;
    line.style.display = knob.style.display = on ? '' : 'none';
    if (on) {
      const a = (draft.minutes! / 1440) * 2 * Math.PI - Math.PI / 2;
      // The hand starts outside the centre readout so it never crosses the digits.
      for (const [k, v] of [['x1', c + 0.62 * r * Math.cos(a)], ['y1', c + 0.62 * r * Math.sin(a)], ['x2', c + r * Math.cos(a)], ['y2', c + r * Math.sin(a)]] as const) line.setAttribute(k, String(v));
      knob.setAttribute('cx', String(c + r * Math.cos(a)));
      knob.setAttribute('cy', String(c + r * Math.sin(a)));
    }
    time.textContent = on ? `${two(Math.floor(draft.minutes! / 60))}:${two(draft.minutes! % 60)}` : '--:--';
    when.textContent = `${shortDay(draft.day)}${draftTime() !== null ? ` · in ${countdown(draftTime()!, Date.now())}` : ''}`;
    // Keep the sheet's readout in step while dragging.
    const readoutTime = document.querySelector<HTMLElement>('#readout .dot.big');
    if (readoutTime) readoutTime.textContent = time.textContent;
  };
  const set = (event: PointerEvent) => {
    const box = s.getBoundingClientRect();
    const x = event.clientX - box.left - box.width / 2, y = event.clientY - box.top - box.height / 2;
    const turn = (Math.atan2(y, x) + Math.PI / 2 + 2 * Math.PI) % (2 * Math.PI);
    draft.minutes = (Math.round((turn / (2 * Math.PI)) * 1440 / 5) * 5) % 1440;
    draw();
  };
  s.addEventListener('pointerdown', event => { event.preventDefault(); s.setPointerCapture(event.pointerId); set(event); });
  s.addEventListener('pointermove', event => { if (s.hasPointerCapture(event.pointerId)) set(event); });
  const finish = (event: PointerEvent) => {
    if (!s.hasPointerCapture(event.pointerId)) return;
    s.releasePointerCapture(event.pointerId);
    renderSheet();
    refreshPreview();
  };
  s.addEventListener('pointerup', finish);
  s.addEventListener('pointercancel', finish);
  face.append(s, centre);
  wrap.append(face, el('p', 'unit dial-note', '24-hour dial · midnight at the top, noon at the bottom'));
  draw();
  return wrap;
}

// B2 instrument for Duration: a dot bar, one dot per 5 minutes up to 2 hours.
function durationTrack() {
  const wrap = el('div');
  const track = el('div', 'track');
  for (let i = 1; i <= 24; i++) {
    const dot = el('button', (draft.duration ?? 0) >= i * 5 ? 'on' : '');
    dot.type = 'button';
    dot.setAttribute('aria-label', `${i * 5} minutes`);
    dot.addEventListener('click', event => { event.stopPropagation(); draft.duration = i * 5; renderSheet(); });
    track.append(dot);
  }
  const labels = el('div', 'track-l');
  for (const l of ['0', '30m', '1h', '1h30', '2h']) labels.append(el('span', '', l));
  wrap.append(track, labels);
  return wrap;
}

// B2 instrument for Warn before: a dot timeline up to the start, warnings lit.
function warnTimeline() {
  const marker = draft.kind === 'marker';
  const marks = marker ? [10080, 2880, 1440, 0] : [10080, 2880, 1440, 120, 60, 30, 15, 10, 5];
  const wrap = el('div');
  const track = el('div', 'track');
  for (const m of marks) track.append(el('i', draft.warnings.includes(m) ? 'on' : ''));
  track.append(el('i', 'big'));
  const labels = el('div', 'track-l');
  labels.append(el('span', '', '−1W'), el('span', '', marker ? '09:00 on the day' : 'start'));
  wrap.append(track, labels);
  return wrap;
}

// The Repeat tile's ring: one cycle, with the occurrences in it lit.
function repeatRing(r: Repeat | null) {
  let dots = 12;
  let lit: (i: number) => boolean = i => i === 0;
  if (r?.type === 'fixed') { dots = 12; lit = i => r.minutes < 1440 ? i % Math.max(1, Math.round(12 * r.minutes / 1440)) === 0 : i === 0; }
  else if (r && (r.unit === 'day' || r.unit === 'week')) {
    dots = 7;
    const days = r.weekdays?.length ? r.weekdays : r.unit === 'day' ? [1, 2, 3, 4, 5, 6, 7] : [((new Date(draft.day).getDay() + 6) % 7) + 1];
    lit = i => days.includes(i + 1);
  } else if (r?.unit === 'month') lit = i => i % r.every === 0;
  const s = svg('svg', { width: 54, height: 54, viewBox: '0 0 60 60', 'aria-hidden': 'true' });
  for (let i = 0; i < dots; i++) {
    const a = (i / dots) * 2 * Math.PI - Math.PI / 2;
    s.append(svg('circle', { cx: 30 + 23 * Math.cos(a), cy: 30 + 23 * Math.sin(a), r: lit(i) && r ? 2.9 : 1.8, fill: lit(i) && r ? 'var(--fg)' : 'var(--faint)' }));
  }
  return s;
}

const TILES = ['duration', 'repeat', 'warn', 'notes'] as const;
type TileName = typeof TILES[number];
function tile(name: TileName) {
  const open = openTile === name;
  const t = el('div', `tl${open ? ' w open' : ''}${openTile && !open ? ' faded' : ''}`);
  t.setAttribute('role', 'button');
  t.tabIndex = 0;
  t.addEventListener('click', () => { if (!open) { openTile = name; renderSheet(); refreshPreview(); } });
  const row2 = (...parts: Node[]) => { const r = el('div', 'row2'); r.append(...parts); return r; };
  const big = (text: string, size = '') => el('span', `dot big${size}`, text);
  const presets = (items: [string, boolean, () => void][], ghost = false) => {
    const g = el('div', 'pgrid');
    for (const [label, on, action] of items) g.append(choice(label, on, action, ghost ? 'ghost' : ''));
    return g;
  };
  const marker = draft.kind === 'marker';
  if (name === 'duration') {
    const [n, unit] = draft.duration ? durationText(draft.duration) : ['—', ''];
    t.append(el('span', 'lbl', 'Duration'), row2(big(n), el('span', 'unit', unit)));
    if (open) {
      t.append(durationTrack(), presets([
        ['None', !draft.duration, () => { draft.duration = null; }],
        ...[15, 30, 45, 60, 90, 120, 180].map(m => [durationText(m).join(' ').replace('1 hour', '1h').replace(' hours', 'h'), draft.duration === m, () => { draft.duration = m; }] as [string, boolean, () => void]),
      ]));
    }
  } else if (name === 'repeat') {
    const [n, unit] = repeatText(draft.repeat);
    const value = el('div');
    value.append(big(n, n.length > 3 ? ' small' : ''), el('span', 'unit', unit));
    t.append(el('span', 'lbl', 'Repeat'), row2(value, repeatRing(draft.repeat)));
    if (open) {
      const cal = (unit: 'day' | 'week' | 'month' | 'year', every: number): Repeat => ({ type: 'calendar', unit, every });
      const options: [string, Repeat | null][] = marker
        ? [['Never', null], ['Weekly', cal('week', 1)], ['1 mo', cal('month', 1)], ['Yearly', cal('year', 1)]]
        : [['Never', null], ['8 h', { type: 'fixed', minutes: 480 }], ['Daily', cal('day', 1)], ['Weekly', cal('week', 1)], ['28 d', { type: 'fixed', minutes: 40320 }],
          ['1 mo', cal('month', 1)], ['3 mo', cal('month', 3)], ['4 mo', cal('month', 4)], ['6 mo', cal('month', 6)], ['12 mo', cal('year', 1)]];
      const current = draft.repeat?.type === 'calendar' ? { ...draft.repeat, weekdays: undefined } : draft.repeat;
      t.append(presets(options.map(([label, r]) => [label, sameRepeat(current as Repeat | null, r), () => {
        if (!marker && draft.minutes === null) draft.minutes = 540;
        draft.repeat = r;
      }])));
      const r = draft.repeat;
      if (r?.type === 'calendar' && (r.unit === 'day' || r.unit === 'week')) {
        const days = r.weekdays ?? [];
        const setDays = (d: number[]) => { draft.repeat = { ...r, weekdays: d.length && d.length < 7 ? d : undefined }; };
        const short = el('div', 'row2 spaced');
        const shortcuts = el('span', 'lbl');
        shortcuts.append(choice('Weekdays', days.join() === '1,2,3,4,5', () => setDays([1, 2, 3, 4, 5]), 'link'), document.createTextNode(' · '),
          choice('Weekends', days.join() === '6,7', () => setDays([6, 7]), 'link'), document.createTextNode(' · '),
          choice('All', !days.length, () => setDays([]), 'link'));
        short.append(el('span', 'lbl', 'Days'), shortcuts);
        const row = el('div', 'days');
        WEEKDAYS.forEach((letter, i) => {
          const d = i + 1, on = days.includes(d);
          row.append(choice(letter, on, () => setDays(on ? days.filter(x => x !== d) : [...days, d].sort())));
        });
        t.append(short, row);
      }
      if (preview.length) {
        const fmt = (ms: number) => marker ? shortDay(ms) : `${shortDay(ms)} · ${clock(ms)}`;
        t.append(row2(el('span', 'unit', `Next · ${fmt(preview[0])}`), el('span', 'unit', preview.length > 1 ? `then ${preview.slice(1).map(p => weekday(p)).join(', ')}` : '')));
      }
    }
  } else if (name === 'warn') {
    const values = el('div', 'stack');
    const sorted = [...draft.warnings].sort((a, b) => b - a);
    if (sorted.length) for (const w of sorted.slice(0, 2)) values.append(big(spanText(w), ' small'));
    else values.append(big('—'));
    t.append(el('span', 'lbl', 'Warn before'), row2(values, el('span', 'unit', sorted.length ? `${sorted.length} set` : '')));
    if (open) {
      const marks = marker ? [0, 1440, 2880, 10080] : [5, 10, 15, 30, 60, 120, 1440, 2880, 10080];
      t.append(warnTimeline(), presets(marks.map(m => [m === 0 ? 'Day of' : spanText(m).toLowerCase().replace('w', ' wk').replace('d', ' d').replace('h', ' h').replace('m', ' min'), draft.warnings.includes(m), () => {
        draft.warnings = draft.warnings.includes(m) ? draft.warnings.filter(x => x !== m) : [...draft.warnings, m];
      }])));
      if (marker) t.append(el('p', 'unit', 'Counted back from 09:00 on the day'));
    }
  } else {
    t.append(el('span', 'lbl', 'Notes'));
    if (open) {
      const area = el('textarea');
      area.maxLength = 8000; area.rows = 4; area.value = draft.notes;
      area.addEventListener('input', () => { draft.notes = area.value; });
      area.addEventListener('click', event => event.stopPropagation());
      t.append(area);
      setTimeout(() => area.focus());
    } else t.append(el('span', 'notes-line', draft.notes.split('\n')[0] || 'Add notes'));
  }
  if (open) {
    const done = el('button', 'pill', 'Done');
    done.type = 'button';
    done.addEventListener('click', event => { event.stopPropagation(); openTile = null; renderSheet(); });
    t.append(done);
  }
  return t;
}

// The When editor opens under the readout: the dial and time presets. Markers have no time.
function whenEditor() {
  const box = el('div', 'tl w open when');
  const now = draft.minutes ?? Math.ceil((new Date().getHours() * 60 + new Date().getMinutes()) / 30) * 30;
  const presets = el('div', 'pgrid');
  for (const [label, on, action] of [
    ['−30m', false, () => { draft.minutes = (now + 1410) % 1440; }],
    ['07:00', draft.minutes === 420, () => { draft.minutes = 420; }],
    ['09:00', draft.minutes === 540, () => { draft.minutes = 540; }],
    ['12:00', draft.minutes === 720, () => { draft.minutes = 720; }],
    ['18:00', draft.minutes === 1080, () => { draft.minutes = 1080; }],
    ['+30m', false, () => { draft.minutes = (now + 30) % 1440; }],
    ['No time', draft.minutes === null, () => { draft.minutes = null; draft.repeat = null; }],
  ] as [string, boolean, () => void][]) presets.append(choice(label, on, action));
  const done = el('button', 'pill', 'Done');
  done.type = 'button';
  done.addEventListener('click', event => { event.stopPropagation(); openTile = null; renderSheet(); });
  box.append(dial(), presets, done);
  return box;
}

function keys() {
  const wrap = el('div', 'keys');
  const names = { alarm: 'Alarm', reminder: 'Reminder', marker: 'Marker' } as const;
  for (const k of ['alarm', 'reminder', 'marker'] as const) {
    const b = choice(names[k], draft.kind === k, () => {
      draft.kind = k;
      // An alarm needs a time; offer the next half hour.
      if (k === 'alarm' && draft.minutes === null) draft.minutes = Math.ceil((new Date().getHours() * 60 + new Date().getMinutes() + 1) / 30) * 30 % 1440;
      if (k === 'marker') { draft.minutes = null; draft.warnings = draft.warnings.filter(w => w === 0 || w % 1440 === 0); if (draft.repeat?.type === 'fixed') draft.repeat = null; }
      else draft.warnings = draft.warnings.filter(w => w !== 0);
    }, 'key');
    b.prepend(el('i', `node${k === 'marker' ? ' mk' : k === 'alarm' ? ' al' : ''}`));
    wrap.append(b);
  }
  return wrap;
}

function dayChips() {
  const chips = el('div', 'chips');
  const today = startOfDay(Date.now());
  for (const [day, label] of [[today, 'Today'], [addDays(today, 1), shortDay(addDays(today, 1))], [addDays(today, 2), shortDay(addDays(today, 2))]] as const) {
    chips.append(choice(label, draft.day === day, () => { draft.day = day; }, 'pill'));
  }
  const picked = ![today, addDays(today, 1), addDays(today, 2)].includes(draft.day);
  chips.append(choice(picked ? shortDay(draft.day) : 'Pick…', picked, () => {
    const pick = $<HTMLInputElement>('#pick-date');
    pick.value = isoDate(draft.day);
    pick.showPicker?.();
  }, 'pill'));
  return chips;
}
$<HTMLInputElement>('#pick-date').addEventListener('change', event => {
  const value = (event.target as HTMLInputElement).value;
  if (value) { draft.day = new Date(`${value}T00:00`).getTime(); renderSheet(); refreshPreview(); }
});

function renderSheet() {
  const marker = draft.kind === 'marker';
  $('#sheet-label').textContent = editing ? 'Edit entry' : 'New entry';
  $('#keys').replaceWith(Object.assign(keys(), { id: 'keys' }));
  const time = draftTime();
  const readout = $<HTMLButtonElement>('#readout');
  readout.classList.toggle('open', openTile === 'when');
  readout.disabled = marker;
  readout.setAttribute('aria-expanded', String(openTile === 'when'));
  readout.replaceChildren(el('span', 'dot big', marker ? 'All day' : time !== null ? clock(time) : '--:--'),
    el('span', 'lbl right', `${longDay(draft.day)}${time !== null ? `\nin ${countdown(time, Date.now())}` : marker ? '' : '\ntap to set a time'}`));
  $('#when-edit').replaceChildren(...(openTile === 'when' && !marker ? [whenEditor()] : []));
  $('#day-chips').replaceWith(Object.assign(dayChips(), { id: 'day-chips' }));
  $<HTMLDivElement>('#tiles').replaceChildren(...TILES.map(tile));
  $('#sheet-submit').textContent = editing ? 'Save' : `Add ${draft.kind}`;
  // Desktop alarms ring from the running app only; say so where it matters.
  $('#delivery-hint').textContent = deliveryHint + (draft.kind === 'alarm' && !android ? ' Alarms ring only while NothingToDo is running.' : '');
}

function openSheet(id: string | null) {
  const fromBar = id ? null : parse(quick.value);
  editing = id;
  const record = id ? data.records[id] : null;
  const task = record?.task ?? null;
  const barTime = fromBar ? quickTarget(fromBar) : null;
  const marked = record ? markerDay(record) : null;
  const timed = task?.remindAt ?? barTime;
  draft = {
    kind: task?.kind === 'marker' || task?.kind === 'alarm' ? task.kind : fromBar?.alarm ? 'alarm' : 'reminder',
    day: marked ?? (timed !== null ? startOfDay(timed) : fromBar?.day ?? Math.max(selectedDay, startOfDay(Date.now()))),
    minutes: timed !== null && task?.kind !== 'marker' ? new Date(timed).getHours() * 60 + new Date(timed).getMinutes() : null,
    duration: task?.durationMinutes ?? fromBar?.duration ?? null,
    repeat: task?.repeat ?? fromBar?.repeat ?? null,
    warnings: task?.warnings ?? fromBar?.warnings ?? [],
    notes: task?.notes ?? '',
  };
  titleInput.value = task?.title ?? fromBar?.title ?? '';
  openTile = null;
  renderSheet();
  refreshPreview();
  sheet.hidden = false;
  titleInput.focus();
}
function resetEditor() { editing = null; sheet.hidden = true; openTile = null; }

// Drag the sheet down from its top, or tap the handle, to close it.
let dragY: number | null = null;
form.addEventListener('touchstart', event => {
  dragY = form.scrollTop <= 0 && event.touches[0].clientY - form.getBoundingClientRect().top < 90 ? event.touches[0].clientY : null;
}, { passive: true });
form.addEventListener('touchend', event => {
  if (dragY !== null && event.changedTouches[0].clientY - dragY > 90) resetEditor();
  dragY = null;
});
$('#sheet-handle').addEventListener('click', resetEditor);
$('#readout').addEventListener('click', () => { openTile = openTile === 'when' ? null : 'when'; renderSheet(); });

// Android's back gesture asks this first (MainActivity.kt): close what is open and return true,
// or return false to let back leave the app.
(window as unknown as { nothingToDoBack: () => boolean }).nothingToDoBack = () => {
  if (!settings.hidden) { closeSettings(); return true; }
  if (!monthSheet.hidden) { monthSheet.hidden = true; return true; }
  if (!sheet.hidden && openTile) { openTile = null; renderSheet(); return true; }
  if (!sheet.hidden) { resetEditor(); return true; }
  if (quick.value) { quick.value = ''; quick.blur(); render(); return true; }
  if (expanded) { expanded = null; render(); return true; }
  return false;
};
$('#open-sheet').addEventListener('click', () => openSheet(null));
sheet.addEventListener('click', event => { if (event.target === sheet) resetEditor(); });
form.addEventListener('submit', event => {
  event.preventDefault();
  void run(async () => {
    if (!titleInput.value.trim()) { titleInput.focus(); status('Give it a title first.'); return; }
    const task = draftTask();
    const wasNew = !editing;
    await save(editing, task);
    if (wasNew) quick.value = '';
    resetEditor();
    goToDay(draft.day);
    await askForNotifications(task.remindAt ?? (task.warnings?.length ? 1 : null));
  });
});
// Android schedules reminders with the OS; desktop shows them from the running app.
const android = /Android/i.test(navigator.userAgent);
const deliveryHint = android
  ? 'Times use this device’s timezone. Reminders, warnings and alarms fire even when the app is closed.'
  : 'Times use this device’s timezone. Reminders need the app running; closing the window keeps it in the tray.';
$('#delivery-hint').textContent = deliveryHint;

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
    (window as unknown as { nothingToDoBack: () => boolean }).nothingToDoBack();
    return;
  }
  if (typing || event.ctrlKey || event.metaKey || event.altKey || !sheet.hidden || !settings.hidden) return;
  if (event.key === 'n') { event.preventDefault(); quick.focus(); }
  if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
    goToDay(addDays(selectedDay, event.key === 'ArrowLeft' ? -1 : 1));
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
  await listen<Ringing | null>('alarm-ringing', event => { ringing = event.payload; render(); });
  ringing = await invoke<Ringing | null>('ringing_alarm');
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
