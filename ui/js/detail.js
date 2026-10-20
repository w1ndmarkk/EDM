// Per-download window (IDM style): status, segment bar and connection table; turns into a
// "completed" view when the download finishes.
'use strict';

const id = new URLSearchParams(location.search).get('id');
const WIDTH = 580;
const FULL_HEIGHT = 560;
let item = null;
let settings = null;
// The segment map, connection table and yt-dlp output open on request ("Show Details >>"), like IDM.
let showDetails = false;
let shown = false;
let mode = '';
let title = '';
let logPending = false;

const closeWindow = () => TAURI.window.getCurrentWindow().close();

async function init() {
  const b = await invoke('bootstrap');
  await initI18n(b.lang);
  settings = b.settings;
  $('#doneIcon').innerHTML = icon('check');
  $('#autoClose').checked = settings.autoCloseDetail;
  $('#autoClose').onchange = (e) => invoke('set_settings', { patch: { autoCloseDetail: e.target.checked } });
  $('#toggleDetails').onclick = () => { showDetails = !showDetails; render(); fit(); };
  $('#doneClose').onclick = closeWindow;
  $('#doneReveal').onclick = () => invoke('download_action', { ids: [id], action: 'reveal' });
  $('#doneOpen').onclick = async () => {
    try {
      await invoke('download_action', { ids: [id], action: 'open' });
      closeWindow();
    } catch (e) {
      // The window stays: the reason is shown, and "Show in Folder" is still there.
      $('#doneError').textContent = t('Could not open the file: {0}', String(e));
      $('#doneError').classList.remove('hidden');
      fit();
    }
  };
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && mode === 'completed') closeWindow();
    if (e.key === 'Enter' && mode === 'completed') $('#doneOpen').click();
  });

  update(await invoke('get_item', { id }));
  listen('items', (items) => update(items.find((i) => i.id === id) || null));
  listen('settings', (s) => { settings = s; $('#autoClose').checked = s.autoCloseDetail; });
  window.addEventListener('resize', () => drawBar());
}

function update(next) {
  const wasCompleted = item && item.state === 'completed';
  item = next;
  if (item && item.state === 'completed' && !wasCompleted && mode === 'active' && settings.autoCloseDetail) {
    closeWindow();
    return;
  }
  render();
}

/** Sizes the window to the current view (compact when details are hidden or the download is done). */
function fit() {
  requestAnimationFrame(() => {
    let h = FULL_HEIGHT;
    if (mode !== 'active' || !showDetails) {
      const view = mode === 'completed' ? $('#completed') : mode === 'removed' ? $('#removed') : $('#active');
      const prev = view.style.height;
      view.style.height = 'auto';
      h = Math.ceil(view.scrollHeight) + 2;
      view.style.height = prev;
    }
    // The first fit also shows the window (it is created hidden), centred at its final size.
    invoke('window_fit', { width: window.innerWidth || WIDTH, height: h, show: !shown });
    shown = true;
  });
}

function setTitle(text) {
  if (text !== title) {
    title = text;
    invoke('window_set_title', { title: text });
  }
}

function infoRows(container, rows) {
  container.innerHTML = '';
  for (const [k, v, cls] of rows) {
    container.append(el('div', { class: 'k', text: k }), el('div', { class: 'v selectable ' + (cls || ''), text: v, title: v }));
  }
}

function statusLine(it) {
  if (it.state === 'downloading') {
    if (it.kind === 'video') return it.stage || t('Downloading with yt-dlp…');
    return t('Receiving data…');
  }
  if (it.state === 'failed') return it.errorMessage || t('Error');
  return stateLabel(it.state);
}

function render() {
  const newMode = !item ? 'removed' : item.state === 'completed' ? 'completed' : 'active';
  const modeChanged = newMode !== mode;
  mode = newMode;
  $('#active').classList.toggle('hidden', mode !== 'active');
  $('#completed').classList.toggle('hidden', mode !== 'completed');
  $('#removed').classList.toggle('hidden', mode !== 'removed');
  if (mode === 'removed') { setTitle('EDM'); if (modeChanged) fit(); return; }

  const pct = Math.floor(progressOf(item) * 100);
  if (mode === 'completed') {
    setTitle(item.fileName);
    $('#doneName').textContent = item.fileName;
    const folder = (item.finalPath || item.destination).replace(/[\\/][^\\/]*$/, '');
    const rows = [[t('Size'), Fmt.size(item.totalBytes)], [t('Location'), folder], [t('URL'), item.url]];
    if (item.completedAt) rows.push([t('Finished'), Fmt.dateTime(item.completedAt)]);
    infoRows($('#doneInfo'), rows);
    if (modeChanged) fit();
    return;
  }

  setTitle((item.totalBytes > 0 ? `${pct}% · ` : '') + item.fileName);
  const resume = item.kind === 'video' ? 'yt-dlp' : item.supportsRanges ? t('Yes') : ['queued', 'probing'].includes(item.state) ? '?' : t('No');
  const eta = etaOf(item);
  infoRows($('#info'), [
    [t('URL'), item.url],
    [t('Status'), statusLine(item), item.state === 'failed' ? 'failed' : ''],
    [t('File size'), item.totalBytes > 0 ? Fmt.size(item.totalBytes) : t('Unknown')],
    [t('Downloaded'), item.totalBytes > 0 ? `${Fmt.size(item.receivedBytes)}  (${pct}%)` : Fmt.size(item.receivedBytes)],
    [t('Transfer rate'), item.state === 'downloading' ? Fmt.speed(item.speed) : '—'],
    [t('Time left'), eta != null ? Fmt.duration(eta) : '—'],
    [t('Resume capability'), resume],
    [t('Save to'), item.finalPath || item.destination],
  ]);
  $('#bar').style.width = (item.totalBytes > 0 ? pct : 0) + '%';
  $('#toggleDetails').textContent = showDetails ? t('<< Hide Details') : t('Show Details >>');

  const primary = $('#primaryBtn'), secondary = $('#secondaryBtn');
  primary.classList.toggle('hidden', item.state === 'cancelled');
  if (['downloading', 'probing', 'queued'].includes(item.state)) {
    primary.textContent = t('Pause');
    primary.classList.remove('primary');
    primary.onclick = () => invoke('download_action', { ids: [id], action: 'pause' });
    secondary.textContent = t('Cancel');
    // Cancel means cancel: the window closes and the item stays in the list as "Cancelled".
    secondary.onclick = async () => { await invoke('download_action', { ids: [id], action: 'cancel' }); closeWindow(); };
  } else if (item.state === 'cancelled') {
    secondary.textContent = t('Close');
    secondary.onclick = closeWindow;
  } else {
    primary.textContent = t('Resume');
    primary.classList.add('primary');
    primary.onclick = () => invoke('download_action', { ids: [id], action: 'resume' });
    secondary.textContent = t('Remove from List');
    secondary.onclick = async () => { await invoke('download_action', { ids: [id], action: 'remove' }); closeWindow(); };
  }

  $('#details').classList.toggle('hidden', !showDetails);
  const video = item.kind === 'video';
  $('#fileDetails').classList.toggle('hidden', video);
  $('#videoDetails').classList.toggle('hidden', !video);
  if (showDetails) {
    if (video) refreshLog(); else { drawBar(); renderConnections(); }
  }
  if (modeChanged) fit();
}

function drawBar() {
  if (!item || mode !== 'active' || item.kind === 'video' || !showDetails) return;
  drawSegments($('#segbar'), item.segments, item.totalBytes, progressOf(item), item.state === 'paused' ? cssVar('--text-2') : cssVar('--accent'));
}

function renderConnections() {
  let rows;
  if (!item.segments.length) {
    if (item.state === 'queued') rows = [];
    else {
      const info = { probing: t('Sending GET…'), downloading: t('Receiving data… (single stream)'), paused: t('Paused') }[item.state] || stateLabel(item.state);
      rows = [[1, item.receivedBytes, 0, info]];
    }
  } else {
    rows = item.segments.map((s, i) => {
      let info;
      if (s.received >= s.end - s.start + 1) info = t('Completed');
      else if (item.state === 'downloading') info = s.received === 0 ? t('Sending GET…') : t('Receiving data…');
      else if (item.state === 'paused') info = t('Paused');
      else info = stateLabel(item.state);
      return [i + 1, s.received, s.start, info];
    });
  }
  const tbody = $('#conns');
  while (tbody.rows.length > rows.length) tbody.deleteRow(-1);
  rows.forEach((r, i) => {
    const tr = tbody.rows[i] || tbody.insertRow();
    while (tr.cells.length < 4) tr.insertCell();
    const vals = [String(r[0]), Fmt.size(r[1]), Fmt.size(r[2]), r[3]];
    vals.forEach((v, j) => { if (tr.cells[j].textContent !== v) tr.cells[j].textContent = v; });
  });
}

async function refreshLog() {
  if (logPending) return;
  logPending = true;
  try {
    const text = await invoke('get_item_log', { id });
    const pre = $('#log');
    const value = text || t('Waiting…');
    if (pre.textContent !== value) {
      const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
      pre.textContent = value;
      if (atBottom) pre.scrollTop = pre.scrollHeight;
    }
  } finally {
    setTimeout(() => { logPending = false; }, 400);
  }
}

init();
