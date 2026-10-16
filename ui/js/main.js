// Main window: sidebar categories, sortable download list, toolbar, New Download and first-run dialogs.
'use strict';

const state = {
  items: [],
  settings: null,
  server: null,
  defaults: {},
  nav: 'all',
  categoriesOpen: true,
  search: '',
  sort: { key: 'date', asc: false },
  selection: new Set(),
  anchor: null,
  addSegments: 0,
};
const rows = new Map(); // id -> { tr, cells, last }
const CATEGORIES = ['compressed', 'document', 'music', 'program', 'video'];

// ---------- Startup ----------
async function init() {
  const b = await invoke('bootstrap');
  await initI18n(b.lang);
  Object.assign(state, { items: b.items, settings: b.settings, server: b.server, defaults: { captureTypes: b.defaultCaptureTypes } });

  setupToolbar();
  buildNav();
  setupTable();
  setupKeyboard();
  setupDrop();
  setupAddDialog();
  setupPlaylistDialog();
  refresh();
  renderStatus();

  listen('items', (items) => { state.items = items; refresh(); });
  listen('server', (s) => { state.server = s; renderStatus(); });
  listen('settings', (s) => { state.settings = s; onSettings.forEach((f) => f()); });
  listen('main-action', runAction);

  if (!state.settings.onboardingDone) openOnboarding();
  else runAction(new URLSearchParams(location.search).get('action'));
  setInterval(() => renderList(true), 30000); // relative "Date Added"
}
const onSettings = [];

/** Requests from the tray, the browser extension or a second launch: "add", "playlist", "playlist:<url>". */
function runAction(a) {
  if (!a) return;
  if (a === 'add') openAdd();
  else if (a === 'playlist' || a.startsWith('playlist:')) openPlaylist(a.slice('playlist:'.length));
}

function refresh() {
  const ids = new Set(state.items.map((i) => i.id));
  for (const id of state.selection) if (!ids.has(id)) state.selection.delete(id);
  updateNav();
  renderList();
  renderStatus();
}

// ---------- Toolbar ----------
function setupToolbar() {
  const label = (btn, ic, text) => { btn.innerHTML = icon(ic) + (text ? `<span>${text}</span>` : ''); };
  label($('#btnAdd'), 'add', t('Add URL'));
  label($('#btnPlaylist'), 'playlist', t('Playlist'));
  label($('#btnStart'), 'play', t('Start Queue'));
  label($('#btnStop'), 'stop', t('Stop All'));
  label($('#btnClear'), 'trash', t('Clear Finished'));
  label($('#btnSettings'), 'gear', '');
  $('.search-icon').innerHTML = icon('search');
  $('#btnAdd').onclick = openAdd;
  $('#btnPlaylist').onclick = () => openPlaylist();
  $('#btnStart').onclick = () => invoke('queue_action', { action: 'resumeAll' });
  $('#btnStop').onclick = () => invoke('queue_action', { action: 'pauseAll' });
  $('#btnClear').onclick = () => invoke('queue_action', { action: 'clearFinished' });
  $('#btnSettings').onclick = () => invoke('show_settings', { tab: null });
  $('#search').addEventListener('input', (e) => { state.search = e.target.value; renderList(); });
}

// ---------- Sidebar ----------
const navButtons = new Map();

function buildNav() {
  const nav = $('#nav');
  const make = (key, ic, text, cls = '') => {
    const b = el('button', { class: `nav-item ${cls}` }, el('span', { html: icon(ic) }), el('span', { class: 'label', text }), el('span', { class: 'count' }));
    b.onclick = () => { state.nav = key; updateNav(); renderList(); };
    navButtons.set(key, b);
    return b;
  };
  const all = make('all', 'all', t('All Downloads'));
  const twisty = el('span', { class: 'twisty', html: icon('chevron') });
  twisty.onclick = (e) => { e.stopPropagation(); state.categoriesOpen = !state.categoriesOpen; updateNav(); };
  all.append(twisty);
  nav.append(all);
  for (const c of CATEGORIES) nav.append(make('cat:' + c, c, categoryLabel(c), 'sub'));
  nav.append(make('finished', 'check', t('Finished')));
  nav.append(make('unfinished', 'clock', t('Unfinished')));
}

function updateNav() {
  const counts = { all: state.items.length, finished: 0, unfinished: 0 };
  for (const it of state.items) {
    counts['cat:' + it.category] = (counts['cat:' + it.category] || 0) + 1;
    if (it.state === 'completed') counts.finished++; else counts.unfinished++;
  }
  for (const [key, b] of navButtons) {
    b.classList.toggle('active', state.nav === key);
    $('.count', b).textContent = counts[key] || '';
    if (key.startsWith('cat:')) b.classList.toggle('hidden', !state.categoriesOpen);
  }
  $('.twisty', navButtons.get('all')).classList.toggle('open', state.categoriesOpen);
}

// ---------- List ----------
function filtered() {
  let list = state.items;
  if (state.nav.startsWith('cat:')) list = list.filter((i) => i.category === state.nav.slice(4));
  else if (state.nav === 'finished') list = list.filter((i) => i.state === 'completed');
  else if (state.nav === 'unfinished') list = list.filter((i) => i.state !== 'completed');
  const q = state.search.trim().toLocaleLowerCase();
  if (q) list = list.filter((i) => i.fileName.toLocaleLowerCase().includes(q) || i.url.toLocaleLowerCase().includes(q));
  const { key, asc } = state.sort;
  const val = {
    name: (i) => i.fileName.toLocaleLowerCase(),
    size: (i) => i.totalBytes,
    status: (i) => i.state,
    speed: (i) => i.speed,
    date: (i) => i.createdAt,
  }[key];
  return [...list].sort((a, b) => {
    const x = val(a), y = val(b);
    const r = typeof x === 'string' ? x.localeCompare(y) : x - y;
    return asc ? r : -r;
  });
}

function setupTable() {
  $$('#list th[data-sort]').forEach((th) => {
    th.onclick = () => {
      const key = th.dataset.sort;
      state.sort = state.sort.key === key ? { key, asc: !state.sort.asc } : { key, asc: key === 'name' };
      renderList();
    };
  });
  $('#tableWrap').addEventListener('mousedown', (e) => {
    if (e.target.closest('tr') || e.button !== 0) return;
    state.selection.clear();
    renderList();
  });
  $('#tableWrap').addEventListener('contextmenu', (e) => {
    if (e.target.closest('tbody tr')) return;
    e.preventDefault();
    state.selection.clear();
    renderList();
    openMenu(e.clientX, e.clientY);
  });
}

function statusText(it) {
  switch (it.state) {
    case 'completed': return t('Completed');
    case 'queued': return t('Added');
    case 'failed': return t('Error: {0}', it.errorMessage || '');
    case 'cancelled': return t('Cancelled');
    default: {
      const pct = it.totalBytes > 0 ? `${Math.floor(progressOf(it) * 100)}% ` : '';
      if (it.kind === 'video' && it.state === 'downloading' && it.stage) return pct + it.stage;
      return pct + stateLabel(it.state);
    }
  }
}

function createRow(it) {
  const tr = el('tr');
  tr.dataset.id = it.id;
  const cells = {
    icon: el('span', { class: 'ficon' }),
    name: el('span', { class: 'ellipsis' }),
    sub: el('span', { class: 'sub ellipsis' }),
    size: el('td', { class: 'num' }),
    statusText: el('div', { class: 'text' }),
    bar: el('canvas', { class: 'segbar' }),
    speed: el('td', { class: 'num' }),
    eta: el('td', { class: 'num' }),
    date: el('td', { class: 'muted' }),
  };
  cells.status = el('td', { class: 'cell-status' }, cells.statusText, cells.bar);
  tr.append(
    el('td', {}, el('div', { class: 'cell-name' }, cells.icon, el('div', { class: 'names' }, cells.name, cells.sub))),
    cells.size, cells.status, cells.speed, cells.eta, cells.date);
  tr.addEventListener('mousedown', (e) => {
    if (e.button === 2 && state.selection.has(it.id)) return;
    if (e.button !== 0 && e.button !== 2) return;
    select(it.id, e.button === 0 && e.ctrlKey, e.button === 0 && e.shiftKey);
  });
  tr.addEventListener('dblclick', () => invoke('show_detail', { id: it.id }));
  tr.addEventListener('contextmenu', (e) => { e.preventDefault(); openMenu(e.clientX, e.clientY); });
  return { tr, cells, last: {} };
}

function setIf(r, key, value, apply) {
  if (r.last[key] !== value) { r.last[key] = value; apply(value); }
}

function updateRow(r, it, force) {
  const c = r.cells;
  setIf(r, 'cat', it.category, (v) => { c.icon.innerHTML = icon(v); c.sub.textContent = categoryLabel(v); });
  setIf(r, 'name', it.fileName, (v) => { c.name.textContent = v; c.name.title = v; });
  const size = it.totalBytes > 0 ? Fmt.size(it.totalBytes) : it.receivedBytes > 0 ? Fmt.size(it.receivedBytes) : '—';
  setIf(r, 'size', size, (v) => { c.size.textContent = v; });
  const st = statusText(it);
  setIf(r, 'status', st, (v) => { c.statusText.textContent = v; c.statusText.title = v; });
  setIf(r, 'failed', it.state === 'failed', (v) => c.status.classList.toggle('failed', v));
  const showBar = ['downloading', 'paused', 'probing'].includes(it.state) || (it.state === 'queued' && it.receivedBytes > 0);
  setIf(r, 'showBar', showBar, (v) => c.bar.classList.toggle('hidden', !v));
  if (showBar) {
    const sig = `${it.state}|${it.totalBytes}|${it.receivedBytes}|${it.segments.length}`;
    if (force || r.last.bar !== sig) {
      r.last.bar = sig;
      const color = it.state === 'paused' ? cssVar('--warning') : it.state === 'failed' ? cssVar('--danger') : cssVar('--accent');
      drawSegments(c.bar, it.segments, it.totalBytes, progressOf(it), color);
    }
  }
  setIf(r, 'speed', it.state === 'downloading' && it.speed > 0 ? Fmt.speed(it.speed) : '', (v) => { c.speed.textContent = v; });
  const eta = it.state === 'downloading' ? etaOf(it) : null;
  setIf(r, 'eta', eta != null ? Fmt.duration(eta) : '', (v) => { c.eta.textContent = v; });
  const date = Fmt.relative(it.createdAt);
  if (force || r.last.date !== date) { r.last.date = date; c.date.textContent = date; c.date.title = Fmt.dateTime(it.createdAt); }
  setIf(r, 'sel', state.selection.has(it.id), (v) => r.tr.classList.toggle('selected', v));
}

function renderList(force = false) {
  const list = filtered();
  const tbody = $('#rows');
  const visible = new Set(list.map((i) => i.id));
  for (const [id, r] of rows) {
    if (!visible.has(id)) { r.tr.remove(); rows.delete(id); }
  }
  let prev = null;
  for (const it of list) {
    let r = rows.get(it.id);
    if (!r) { r = createRow(it); rows.set(it.id, r); }
    const expected = prev ? prev.nextSibling : tbody.firstChild;
    if (expected !== r.tr) tbody.insertBefore(r.tr, expected);
    updateRow(r, it, force || !r.tr.isConnected);
    prev = r.tr;
  }
  $$('#list th[data-sort]').forEach((th) => {
    th.classList.toggle('sorted', th.dataset.sort === state.sort.key);
    th.classList.toggle('asc', th.dataset.sort === state.sort.key && state.sort.asc);
  });
  const empty = list.length === 0;
  $('#empty').classList.toggle('hidden', !empty);
  if (empty) {
    $('.empty-icon').innerHTML = icon('download');
    $('#emptyTitle').textContent = state.search ? t('No matching downloads') : t('No downloads in this view');
    $('#emptyText').textContent = state.search ? '' : t('Download a file from your browser, drop a link here,\nor press Ctrl+N to enter a URL.');
  }
}

function select(id, toggle, range) {
  if (range && state.anchor) {
    const ids = filtered().map((i) => i.id);
    const a = ids.indexOf(state.anchor), b = ids.indexOf(id);
    if (a >= 0 && b >= 0) {
      state.selection = new Set(ids.slice(Math.min(a, b), Math.max(a, b) + 1));
      renderList();
      return;
    }
  }
  if (toggle) {
    if (state.selection.has(id)) state.selection.delete(id); else state.selection.add(id);
  } else {
    state.selection = new Set([id]);
  }
  state.anchor = id;
  renderList();
}

// ---------- Context menu ----------
function openMenu(x, y) {
  const menu = $('#menu');
  menu.innerHTML = '';
  const ids = Array.from(state.selection);
  const items = state.items.filter((i) => state.selection.has(i.id));
  const add = (text, ic, fn, cls = '') => menu.append(el('button', { class: cls, html: icon(ic) + `<span>${escapeHtml(text)}</span>`, onclick: () => { closeMenu(); fn(); } }));
  const sep = () => menu.append(el('hr'));
  const act = (action) => invoke('download_action', { ids, action });

  if (!items.length) {
    add(t('Add URL…'), 'add', openAdd);
    add(t('Download Playlist…'), 'playlist', () => openPlaylist());
  } else {
    add(t('Open Download Window'), 'window', () => ids.forEach((id) => invoke('show_detail', { id })));
    sep();
    if (items.some((i) => isActive(i.state) || i.state === 'queued')) {
      add(t('Pause'), 'pause', () => act('pause'));
      add(t('Cancel'), 'stop', () => act('cancel'));
    }
    if (items.some((i) => ['paused', 'failed', 'cancelled'].includes(i.state))) add(t('Resume / Retry'), 'play', () => act('resume'));
    if (items.length === 1) {
      const it = items[0];
      sep();
      add(t('Copy URL'), 'copy', () => invoke('copy_url', { id: it.id }));
      if (it.state === 'completed') {
        add(t('Open'), 'open', () => act('open'));
        add(t('Show in Folder'), 'folder', () => act('reveal'));
      }
    }
    sep();
    add(t('Remove from List'), 'list', () => { act('remove'); state.selection.clear(); });
    if (items.some((i) => i.state === 'completed')) {
      add(t('Move File to Recycle Bin and Remove'), 'trash', () => { act('trash'); state.selection.clear(); }, 'danger');
    }
  }
  menu.classList.remove('hidden');
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, window.innerWidth - r.width - 4) + 'px';
  menu.style.top = Math.min(y, window.innerHeight - r.height - 4) + 'px';
}

function closeMenu() { $('#menu').classList.add('hidden'); }
function escapeHtml(s) { return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c])); }
document.addEventListener('mousedown', (e) => { if (!e.target.closest('#menu')) closeMenu(); });
window.addEventListener('blur', closeMenu);
window.addEventListener('resize', closeMenu);

// ---------- Keyboard ----------
function setupKeyboard() {
  document.addEventListener('keydown', async (e) => {
    const inField = e.target.closest('input, textarea, select');
    const dialogOpen = document.querySelector('dialog[open]');
    if (e.key === 'Escape') { closeMenu(); if (!inField && !dialogOpen) { state.selection.clear(); renderList(); } return; }
    if (dialogOpen) return;
    if (e.ctrlKey && (e.key === 'n' || e.key === 'N')) { e.preventDefault(); openAdd(); return; }
    if (e.ctrlKey && (e.key === 'l' || e.key === 'L')) { e.preventDefault(); openPlaylist(); return; }
    if (e.ctrlKey && (e.key === 'f' || e.key === 'F')) { e.preventDefault(); $('#search').focus(); return; }
    if (e.ctrlKey && e.key === ',') { e.preventDefault(); invoke('show_settings', { tab: null }); return; }
    if (inField) return;
    if (e.ctrlKey && (e.key === 'v' || e.key === 'V')) {
      // Pasted links go into the New Download dialog; nothing starts until the user clicks Download.
      e.preventDefault();
      openAdd();
    } else if (e.ctrlKey && (e.key === 'a' || e.key === 'A')) {
      e.preventDefault();
      state.selection = new Set(filtered().map((i) => i.id));
      renderList();
    } else if (e.key === 'Delete' && state.selection.size) {
      invoke('download_action', { ids: Array.from(state.selection), action: 'remove' });
      state.selection.clear();
    } else if (e.key === 'Enter' && state.selection.size) {
      state.selection.forEach((id) => invoke('show_detail', { id }));
    }
  });
}

// ---------- Drag and drop of links ----------
function setupDrop() {
  const overlay = $('#dropOverlay');
  const hasText = (e) => e.dataTransfer && Array.from(e.dataTransfer.types).some((x) => x === 'text/uri-list' || x === 'text/plain');
  document.addEventListener('dragover', (e) => {
    if (!hasText(e)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
    overlay.classList.remove('hidden');
  });
  document.addEventListener('dragleave', (e) => { if (!e.relatedTarget) overlay.classList.add('hidden'); });
  document.addEventListener('drop', (e) => {
    overlay.classList.add('hidden');
    if (!hasText(e)) return;
    e.preventDefault();
    const raw = e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain');
    const text = raw.split(/\r?\n/).filter((l) => l && !l.startsWith('#')).join('\n');
    if (text) invoke('add_urls', { text, video: false, segments: null });
  });
}

// ---------- New Download dialog ----------
function setupAddDialog() {
  const dlg = $('#addDialog');
  const text = $('#addText');
  const ok = $('#addOk');
  const updateOk = () => { ok.disabled = !text.value.trim(); };
  const updateSegments = () => {
    const n = state.addSegments;
    $('#addSegmentsLabel').textContent = n === 0 ? t('Connections: default ({0})', state.settings.segmentCount) : t('Connections: {0}', n);
    $('#addSegmentsRow').classList.toggle('hidden', $('#addVideo').checked);
  };
  dlg.updateSegments = updateSegments;
  onSettings.push(updateSegments);
  text.addEventListener('input', updateOk);
  text.addEventListener('keydown', (e) => { if (e.key === 'Enter' && e.ctrlKey) { e.preventDefault(); ok.click(); } });
  $('#addVideo').onchange = updateSegments;
  $('#segMinus').onclick = () => { state.addSegments = Math.max(0, state.addSegments - 1); updateSegments(); };
  $('#segPlus').onclick = () => { state.addSegments = Math.min(16, state.addSegments + 1); updateSegments(); };
  $('#addPaste').onclick = async () => { text.value = await invoke('read_clipboard'); updateOk(); text.focus(); };
  $('#addCancel').onclick = () => dlg.close();
  ok.onclick = async () => {
    if (!text.value.trim()) return;
    await invoke('add_urls', { text: text.value, video: $('#addVideo').checked, segments: state.addSegments || null });
    dlg.close();
  };
  dlg.updateOk = updateOk;
}

async function openAdd() {
  const dlg = $('#addDialog');
  if (dlg.open) return;
  if ($('#onboarding').open) return;
  state.addSegments = 0;
  $('#addVideo').checked = false;
  $('#addText').value = '';
  dlg.updateSegments();
  dlg.updateOk();
  dlg.showModal();
  $('#addText').focus();
  // The links on the clipboard (one or many, also inside copied text), ready to start.
  const links = [...new Set((await invoke('read_clipboard')).match(/https?:\/\/[^\s"'<>]+/gi) || [])].slice(0, 50);
  if (links.length && !$('#addText').value) {
    $('#addText').value = links.join('\n');
    $('#addText').select();
    dlg.updateOk();
  }
}

// ---------- Playlist dialog ----------
const PLAYLIST_QUALITIES = [
  ['settings', 'Video settings ({0})'], ['best', 'Best quality'],
  ['2160', '2160p (4K)'], ['1440', '1440p'], ['1080', '1080p'], ['720', '720p'], ['480', '480p'], ['360', '360p'],
  ['m4a', 'Audio only (M4A)'], ['mp3', 'Audio only (MP3)'],
];
const pl = { list: null, picked: new Set(), token: 0, quality: 'settings', numbered: true };

const looksLikePlaylist = (s) => /^https?:\/\/\S+$/i.test(s) && /[?&]list=|\/playlist\b|\/@[^/]+\/(videos|shorts|streams)|\/(channel|c|user)\/[^/]+\/(videos|shorts|streams)/i.test(s);

function settingsQualityLabel() {
  const s = state.settings;
  if (s.videoFormat && s.videoFormat.trim()) return t('custom');
  if (s.videoMaxHeight === -1) return 'M4A';
  if (s.videoMaxHeight === -2) return 'MP3';
  return s.videoMaxHeight > 0 ? `${s.videoMaxHeight}p` : t('Best quality');
}

function setupPlaylistDialog() {
  const dlg = $('#plDialog');
  const select = $('#plQuality');
  const fillQualities = () => {
    select.innerHTML = '';
    for (const [key, label] of PLAYLIST_QUALITIES) {
      select.append(el('option', { value: key, text: key === 'settings' ? t(label, settingsQualityLabel()) : t(label) }));
    }
    select.value = pl.quality;
  };
  fillQualities();
  onSettings.push(fillQualities);
  select.onchange = () => { pl.quality = select.value; renderPlaylistFoot(); };
  $('#plNumbered').onchange = (e) => { pl.numbered = e.target.checked; };
  $('#plForm').onsubmit = (e) => { e.preventDefault(); fetchPlaylist(); };
  $('#plAll').onchange = (e) => {
    pl.picked = new Set(e.target.checked ? pl.list.entries.filter((x) => x.available).map((x) => x.index) : []);
    $$('#plList input').forEach((cb) => { if (!cb.disabled) cb.checked = e.target.checked; });
    renderPlaylistFoot();
  };
  $('#plCancel').onclick = () => dlg.close();
  dlg.addEventListener('close', () => { pl.token++; });
  $('#plOk').onclick = async () => {
    const entries = pl.list.entries.filter((x) => pl.picked.has(x.index)).map(({ url, title, index }) => ({ url, title, index }));
    if (!entries.length) return;
    $('#plOk').disabled = true;
    try {
      await invoke('add_playlist', { title: pl.list.title, entries, quality: pl.quality, numbered: pl.numbered });
      dlg.close();
      state.nav = pl.quality === 'm4a' || pl.quality === 'mp3' ? 'cat:music' : 'cat:video';
      updateNav(); renderList();
    } catch (e) {
      showPlaylistStatus(String(e), true);
      $('#plOk').disabled = false;
    }
  };
}

function showPlaylistStatus(text, error = false, busy = false) {
  const box = $('#plStatus');
  box.innerHTML = '';
  box.classList.toggle('hidden', !text);
  box.classList.toggle('error', error);
  if (busy) box.append(el('span', { class: 'spinner' }));
  box.append(el('span', { text }));
}

async function fetchPlaylist() {
  const url = $('#plUrl').value.trim();
  if (!url) return;
  const token = ++pl.token;
  pl.list = null;
  $('#plResult').classList.add('hidden');
  $('#plFetch').disabled = true;
  renderPlaylistFoot();
  showPlaylistStatus(t('Reading the playlist… (large playlists take a while)'), false, true);
  try {
    const list = await invoke('fetch_playlist', { url });
    if (token !== pl.token) return;
    if (!list.entries.length) { showPlaylistStatus(t('This playlist has no videos.'), true); return; }
    pl.list = list;
    pl.picked = new Set(list.entries.filter((x) => x.available).map((x) => x.index));
    showPlaylistStatus('');
    renderPlaylist();
  } catch (e) {
    if (token === pl.token) showPlaylistStatus(String(e), true);
  } finally {
    if (token === pl.token) $('#plFetch').disabled = false;
  }
}

function renderPlaylist() {
  const { list } = pl;
  $('#plTitle').textContent = list.uploader && !list.title.includes(list.uploader) ? `${list.title} — ${list.uploader}` : list.title;
  $('#plTitle').title = $('#plTitle').textContent;
  const box = $('#plList');
  box.innerHTML = '';
  for (const it of list.entries) {
    const cb = el('input', { type: 'checkbox' });
    cb.checked = pl.picked.has(it.index);
    cb.disabled = !it.available;
    cb.onchange = () => { if (cb.checked) pl.picked.add(it.index); else pl.picked.delete(it.index); renderPlaylistFoot(); };
    const row = el('label', { class: 'pl-row' + (it.available ? '' : ' off'), title: it.url },
      cb,
      el('span', { class: 'n', text: String(it.index) }),
      el('span', { class: 't ellipsis', text: it.title }),
      el('span', { class: 'd', text: it.duration > 0 ? clock(it.duration) : (it.available ? '' : t('unavailable')) }));
    box.append(row);
  }
  $('#plResult').classList.remove('hidden');
  renderPlaylistFoot();
}

const clock = (sec) => {
  sec = Math.round(sec);
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
  const p = (n) => String(n).padStart(2, '0');
  return h ? `${h}:${p(m)}:${p(s)}` : `${m}:${p(s)}`;
};

function renderPlaylistFoot() {
  const ok = $('#plOk');
  const n = pl.list ? pl.picked.size : 0;
  ok.textContent = n ? t('Download {0} videos', n) : t('Download');
  ok.disabled = !n;
  if (!pl.list) return;
  const available = pl.list.entries.filter((x) => x.available).length;
  $('#plCount').textContent = t('{0} of {1} selected', n, pl.list.entries.length);
  const all = $('#plAll');
  all.checked = n > 0 && n === available;
  all.indeterminate = n > 0 && n < available;
  const audio = pl.quality === 'm4a' || pl.quality === 'mp3' || (pl.quality === 'settings' && state.settings.videoMaxHeight < 0 && !state.settings.videoFormat.trim());
  const base = state.settings.downloadDirectory || '';
  // Category folders have English names on disk (like IDM's).
  const cat = state.settings.organizeByCategory ? (audio ? 'Music' : 'Video') + '\\' : '';
  $('#plDest').textContent = t('Saved to: {0}', `${base}${base.endsWith('\\') ? '' : '\\'}${cat}${folderName(pl.list.title)}`);
  $('#plDest').title = $('#plDest').textContent;
}

// Mirrors store::folder_name (for the preview only).
const folderName = (s) => (String(s || '').trim().slice(0, 80).replace(/[<>:"/\\|?*\x00-\x1f]/g, '_').trim().replace(/[. ]+$/, '') || 'Playlist');

async function openPlaylist(url = '') {
  const dlg = $('#plDialog');
  if ($('#onboarding').open) return;
  if ($('#addDialog').open) $('#addDialog').close();
  if (!dlg.open) {
    pl.token++;
    pl.list = null;
    $('#plUrl').value = '';
    $('#plResult').classList.add('hidden');
    $('#plFetch').disabled = false;
    $('#plNumbered').checked = pl.numbered;
    showPlaylistStatus('');
    renderPlaylistFoot();
    dlg.showModal();
  }
  $('#plUrl').focus();
  if (!url) {
    const clip = (await invoke('read_clipboard')).trim();
    if (looksLikePlaylist(clip)) url = clip;
  }
  if (url) {
    $('#plUrl').value = url;
    fetchPlaylist();
  }
}

// ---------- First run ----------
async function openOnboarding() {
  const dlg = $('#onboarding');
  let step = 0;
  dlg.addEventListener('cancel', (e) => e.preventDefault());
  const syncCapture = buildListEditor($('#obCapture'), {
    key: 'captureTypes',
    label: t('Automatically start downloading the following file types:'),
    hint: t('Separate types with spaces; * and ? are allowed (R0*).'),
    rows: 5,
    defaultValue: state.defaults.captureTypes,
    getSettings: () => state.settings,
  });
  onSettings.push(syncCapture);

  const browsers = (await invoke('browsers_status')).filter((b) => b.installed);
  const list = $('#obBrowsers');
  list.innerHTML = '';
  if (browsers.length) {
    for (const b of browsers) list.append(el('div', { class: 'item' }, el('span', { html: icon('globe') }), el('span', { text: b.name })));
  } else {
    list.append(el('div', { class: 'item muted', text: t('No supported browser was found (Chrome, Edge, Brave, Firefox).') }));
  }

  const show = () => {
    $('#obSubtitle').textContent = step === 0 ? t('Which file types should be captured?') : t('Install the browser extensions?');
    $('#obStep0').classList.toggle('hidden', step !== 0);
    $('#obStep1').classList.toggle('hidden', step !== 1);
    $('#obBack').classList.toggle('hidden', step !== 1);
    $('#obContinue').classList.toggle('hidden', step !== 0);
    $('#obContinue').classList.toggle('primary', step === 0);
    $('#obNotNow').classList.toggle('hidden', step !== 1);
    $('#obSetup').classList.toggle('hidden', step !== 1);
  };
  const finish = async (setup) => {
    await invoke('finish_onboarding');
    dlg.close();
    if (setup) invoke('show_wizard');
  };
  $('#obContinue').onclick = () => { step = 1; show(); };
  $('#obBack').onclick = () => { step = 0; show(); };
  $('#obNotNow').onclick = () => finish(false);
  $('#obSetup').onclick = () => finish(true);
  show();
  dlg.showModal();
}

// ---------- Status bar ----------
function renderStatus() {
  const s = state.server;
  const running = !!(s && s.running);
  $('#bridgeDot').classList.toggle('on', running);
  $('#bridgeText').textContent = running ? t('Browser bridge is running') : (s ? s.statusText : '');
  const unfinished = state.items.filter((i) => i.state !== 'completed').length;
  const speed = state.items.filter((i) => i.state === 'downloading').reduce((a, i) => a + i.speed, 0);
  $('#unfinished').innerHTML = icon('list') + `<span>${unfinished}</span>`;
  $('#totalSpeed').innerHTML = icon('download') + `<span>${escapeHtml(Fmt.speed(speed))}</span>`;
}

init();
