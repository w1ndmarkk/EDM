// Shared helpers for all windows: Tauri bridge, i18n, formatting, icons.
'use strict';

const TAURI = window.__TAURI__;
const invoke = (cmd, args) => TAURI.core.invoke(cmd, args);
const listen = (event, fn) => TAURI.event.listen(event, (e) => fn(e.payload));
const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

// ---------- i18n (English strings are the keys; Turkish comes from i18n/tr.json) ----------
let LANG = 'en';
let TR = {};

async function initI18n(lang) {
  LANG = lang === 'tr' ? 'tr' : 'en';
  document.documentElement.lang = LANG;
  if (LANG === 'tr') {
    try { TR = await (await fetch('i18n/tr.json')).json(); } catch { TR = {}; }
  }
  applyI18n();
}

function t(key, ...args) {
  let s = (LANG === 'tr' && TR[key]) || key;
  args.forEach((a, i) => { s = s.split('{' + i + '}').join(String(a)); });
  return s;
}

function applyI18n(root = document) {
  $$('[data-i18n]', root).forEach((el) => { el.textContent = t(el.dataset.i18n); });
  $$('[data-i18n-title]', root).forEach((el) => { el.title = t(el.dataset.i18nTitle); });
  $$('[data-i18n-placeholder]', root).forEach((el) => { el.placeholder = t(el.dataset.i18nPlaceholder); });
}

// ---------- Formatting (decimal units, like the system file size display) ----------
const Fmt = {
  number(v, digits) {
    return v.toLocaleString(LANG === 'tr' ? 'tr-TR' : 'en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits });
  },
  size(n) {
    if (n == null || n < 0) return '?';
    if (n < 1000) return `${n} ${LANG === 'tr' ? 'bayt' : 'bytes'}`;
    const units = ['KB', 'MB', 'GB', 'TB'];
    let v = n / 1000, i = 0;
    while (v >= 1000 && i < units.length - 1) { v /= 1000; i++; }
    return `${Fmt.number(v, i === 0 ? 0 : i === 1 ? 1 : 2)} ${units[i]}`;
  },
  speed(bps) { return t('{0}/s', Fmt.size(Math.round(bps || 0))); },
  duration(sec) {
    const s = Math.round(sec);
    if (s < 60) return t('{0} s', s);
    if (s < 3600) return t('{0} min {1} s', Math.floor(s / 60), s % 60);
    return t('{0} h {1} min', Math.floor(s / 3600), Math.floor((s % 3600) / 60));
  },
  dateTime(ms) {
    return new Date(ms).toLocaleString(LANG === 'tr' ? 'tr-TR' : 'en-US', { dateStyle: 'medium', timeStyle: 'short' });
  },
  relative(ms) {
    const rtf = new Intl.RelativeTimeFormat(LANG === 'tr' ? 'tr-TR' : 'en-US', { numeric: 'auto' });
    const diff = (ms - Date.now()) / 1000;
    const steps = [[60, 'second'], [3600, 'minute'], [86400, 'hour'], [604800, 'day'], [2629800, 'week'], [31557600, 'month'], [Infinity, 'year']];
    const div = { second: 1, minute: 60, hour: 3600, day: 86400, week: 604800, month: 2629800, year: 31557600 };
    for (const [limit, unit] of steps) {
      if (Math.abs(diff) < limit) return rtf.format(Math.round(diff / div[unit]), unit);
    }
    return '';
  },
};

// ---------- Download helpers ----------
const CATEGORY_LABEL = { compressed: 'Compressed', document: 'Documents', music: 'Music', program: 'Programs', video: 'Video', other: 'Other' };
const STATE_LABEL = { queued: 'Queued', probing: 'Connecting', downloading: 'Downloading', paused: 'Paused', completed: 'Completed', failed: 'Error', cancelled: 'Cancelled' };
const categoryLabel = (c) => t(CATEGORY_LABEL[c] || 'Other');
const stateLabel = (s) => t(STATE_LABEL[s] || s);
const isActive = (s) => s === 'downloading' || s === 'probing';
const progressOf = (it) => (it.totalBytes > 0 ? Math.min(1, it.receivedBytes / it.totalBytes) : 0);
const etaOf = (it) => (it.totalBytes > 0 && it.speed > 1 ? (it.totalBytes - it.receivedBytes) / it.speed : null);

function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Draws how much of each segment has been received (falls back to a plain bar). */
function drawSegments(canvas, segments, total, fallback, color) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth, h = canvas.clientHeight;
  if (!w || !h) return;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  const r = h / 2;
  ctx.save();
  ctx.beginPath();
  ctx.roundRect(0, 0, w, h, r);
  ctx.clip();
  ctx.fillStyle = cssVar('--bar-bg');
  ctx.fillRect(0, 0, w, h);
  ctx.fillStyle = color || cssVar('--accent');
  if (!segments || !segments.length || total <= 0) {
    ctx.fillRect(0, 0, w * (fallback || 0), h);
  } else {
    for (const s of segments) {
      if (s.received > 0) ctx.fillRect((s.start / total) * w, 0, Math.max(1, (s.received / total) * w), h);
    }
    ctx.fillStyle = cssVar('--surface');
    for (const s of segments.slice(1)) ctx.fillRect((s.start / total) * w, 0, 1, h);
  }
  ctx.restore();
}

// ---------- Icons (16px line icons) ----------
const ICON_PATHS = {
  add: '<path d="M8 3v10M3 8h10"/>',
  link: '<path d="M6.5 9.5l3-3"/><path d="M7 4.5l1-1a2.8 2.8 0 014 4l-1 1"/><path d="M9 11.5l-1 1a2.8 2.8 0 01-4-4l1-1"/>',
  play: '<path d="M5 3.5v9l7-4.5z"/>',
  pause: '<path d="M5.5 3.5v9M10.5 3.5v9"/>',
  stop: '<rect x="4" y="4" width="8" height="8" rx="1"/>',
  trash: '<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5"/>',
  gear: '<path d="M6.7 1.8h2.6l.4 1.7 1.2.7 1.7-.5 1.3 2.2-1.3 1.2v1.8l1.3 1.2-1.3 2.2-1.7-.5-1.2.7-.4 1.7H6.7l-.4-1.7-1.2-.7-1.7.5-1.3-2.2 1.3-1.2V7.1L2.1 5.9l1.3-2.2 1.7.5 1.2-.7z"/><circle cx="8" cy="8" r="2"/>',
  folder: '<path d="M2 4.5A1.5 1.5 0 013.5 3H6l1.5 1.5h5A1.5 1.5 0 0114 6v5.5a1.5 1.5 0 01-1.5 1.5h-9A1.5 1.5 0 012 11.5z"/>',
  check: '<circle cx="8" cy="8" r="6"/><path d="M5.5 8.2l1.8 1.8 3.3-3.6"/>',
  clock: '<circle cx="8" cy="8" r="6"/><path d="M8 4.8V8l2.2 1.4"/>',
  image: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><circle cx="6" cy="6.5" r="1"/><path d="M2.5 11.5l3.5-3 2.5 2 2-1.5 3 2.5"/>',
  music: '<path d="M6 12V3.5l7-1.5V10"/><circle cx="4.5" cy="12" r="1.5"/><circle cx="11.5" cy="10" r="1.5"/>',
  video: '<rect x="2" y="3.5" width="9" height="9" rx="1.5"/><path d="M11 7l3-2v6l-3-2"/>',
  program: '<rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1"/><rect x="9" y="2.5" width="4.5" height="4.5" rx="1"/><rect x="2.5" y="9" width="4.5" height="4.5" rx="1"/><rect x="9" y="9" width="4.5" height="4.5" rx="1"/>',
  document: '<path d="M4 2h5l3 3v9H4z"/><path d="M9 2v3h3M6 8h4M6 10.5h4"/>',
  compressed: '<path d="M4 2h5l3 3v9H4z"/><path d="M7 2v1.5M7 5v1.5M7 8v1.5"/><rect x="6" y="10" width="2" height="2"/>',
  other: '<path d="M4 2h5l3 3v9H4z"/><path d="M9 2v3h3"/>',
  all: '<path d="M2 4.5A1.5 1.5 0 013.5 3H6l1.5 1.5h5A1.5 1.5 0 0114 6v5.5a1.5 1.5 0 01-1.5 1.5h-9A1.5 1.5 0 012 11.5z"/>',
  search: '<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>',
  download: '<path d="M8 2.5v8M4.5 7L8 10.5 11.5 7M3 13.5h10"/>',
  list: '<path d="M5.5 4h8M5.5 8h8M5.5 12h8M2.5 4h.01M2.5 8h.01M2.5 12h.01"/>',
  playlist: '<path d="M2.5 4h8M2.5 7.5h8M2.5 11h4.5"/><path d="M10 9.5v4l3.5-2z"/>',
  chevron: '<path d="M6 4l4 4-4 4"/>',
  copy: '<rect x="5" y="5" width="8" height="8" rx="1.5"/><path d="M3 10.5V4a1 1 0 011-1h6.5"/>',
  open: '<path d="M9 2.5h4.5V7M13.5 2.5L7.5 8.5"/><path d="M12 9.5v3a1 1 0 01-1 1H3.5a1 1 0 01-1-1V5a1 1 0 011-1h3"/>',
  window: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M2 6h12"/>',
  globe: '<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2.2 2 9.8 0 12M8 2c-2 2.2-2 9.8 0 12"/>',
  puzzle: '<path d="M3 6h2.2a1.3 1.3 0 112.6 0H10v2.2a1.3 1.3 0 110 2.6V13H3z"/>',
  wand: '<path d="M3 13l7-7M9.5 3.5l.5-1.5.5 1.5 1.5.5-1.5.5-.5 1.5-.5-1.5L8 4zM12.5 7.5l.3-1 .3 1 1 .3-1 .3-.3 1-.3-1-1-.3z"/>',
};

function icon(name, cls = '') {
  return `<svg class="i ${cls}" viewBox="0 0 16 16" aria-hidden="true">${ICON_PATHS[name] || ICON_PATHS.other}</svg>`;
}

function el(tag, attrs = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === 'class') e.className = v;
    else if (k === 'html') e.innerHTML = v;
    else if (k === 'text') e.textContent = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else e.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) {
    if (c == null) continue;
    e.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return e;
}

/**
 * IDM-style list box bound to a space-separated settings field, with a "Default" button
 * (file types and excluded sites; first-run dialog and Settings ▸ File Types). Returns a sync function.
 */
function buildListEditor(container, { key, label, hint, rows, defaultValue, getSettings }) {
  container.innerHTML = '';
  const area = el('textarea', { rows: rows || 4, spellcheck: 'false', class: 'list-box' });
  const save = () => invoke('set_settings', { patch: { [key]: area.value } });
  area.addEventListener('change', save);
  const reset = el('button', { class: 'btn', text: t('Default'), onclick: () => { area.value = defaultValue; save(); } });
  container.append(el('div', { class: 'list-editor' },
    el('label', { class: 'list-label', text: label }),
    area,
    el('div', { class: 'row' }, hint ? el('span', { class: 'caption', text: hint }) : null, el('div', { class: 'spacer' }), reset)));
  const sync = () => { if (document.activeElement !== area) area.value = getSettings()[key]; };
  sync();
  return sync;
}

// No browser chrome in the app: no page context menu (except in text fields), no reload shortcuts.
document.addEventListener('contextmenu', (e) => {
  if (!e.target.closest('input, textarea, .selectable, pre')) e.preventDefault();
});
document.addEventListener('keydown', (e) => {
  if (e.key === 'F5' || (e.ctrlKey && (e.key === 'r' || e.key === 'R' || e.key === 'p' || e.key === 'P' || e.key === 'f' || e.key === 'F'))) {
    if (!(e.key.toLowerCase() === 'f' && e.target.closest('input, textarea'))) e.preventDefault();
  }
});
