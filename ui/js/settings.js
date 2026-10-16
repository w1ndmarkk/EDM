// Settings window: General, File Types, Browser Extensions, Video.
'use strict';

let settings = null;
let server = null;
let tools = null;
let login = null;
let updater = null;
const syncers = [];

const TABS = [
  ['general', 'gear', 'General'],
  ['filetypes', 'document', 'File Types'],
  ['browser', 'puzzle', 'Browser Extensions'],
  ['video', 'video', 'Video'],
];

const patch = (p) => invoke('set_settings', { patch: p });

async function init() {
  const b = await invoke('bootstrap');
  await initI18n(b.lang);
  settings = b.settings;
  server = b.server;

  const nav = $('#tabs');
  for (const [key, ic, label] of TABS) {
    nav.append(el('button', { class: 'tab', 'data-tab': key, html: icon(ic) + `<span>${t(label)}</span>`, onclick: () => showTab(key) }));
  }
  showTab(location.hash.slice(1) || 'general');
  listen('settings-tab', showTab);

  bindSimpleSettings();
  setupGeneral(b.version);
  setupFileTypes(b);
  setupBrowser();
  setupVideo();
  syncAll();

  listen('settings', (s) => { settings = s; syncAll(); });
  listen('server', (s) => { server = s; renderBridge(); renderBrowsers(); });
  listen('tools', (s) => { tools = s; renderTools(); });
  listen('login', (s) => { login = s; renderLogin(); });
  listen('updater', (s) => { updater = s; renderUpdater(); });
}

function showTab(key) {
  if (!TABS.some(([k]) => k === key)) key = 'general';
  $$('.tab').forEach((b) => b.classList.toggle('active', b.dataset.tab === key));
  $$('.page').forEach((p) => p.classList.toggle('active', p.id === 'page-' + key));
  history.replaceState(null, '', '#' + key);
  if (key === 'browser') refreshBrowsers();
  if (key === 'video') invoke('tools_refresh').then((s) => { tools = s; renderTools(); });
}

function syncAll() { syncers.forEach((f) => f()); }

/** Inputs with data-setting map 1:1 to a settings field. */
function bindSimpleSettings() {
  for (const input of $$('[data-setting]')) {
    const key = input.dataset.setting;
    const read = () => {
      if (input.type === 'checkbox') return input.checked;
      if (input.hasAttribute('data-number')) return parseInt(input.value, 10);
      return input.value;
    };
    input.addEventListener('change', () => patch({ [key]: read() }));
    syncers.push(() => {
      if (document.activeElement === input && input.type === 'text') return;
      if (input.type === 'checkbox') input.checked = !!settings[key];
      else input.value = String(settings[key]);
    });
  }
}

// ---------- General ----------
function setupGeneral(version) {
  $('#showDir').onclick = () => invoke('open_folder', { which: 'downloads' });
  $('#chooseDir').onclick = async () => {
    const dir = await TAURI.dialog.open({ directory: true, defaultPath: settings.downloadDirectory });
    if (dir) patch({ downloadDirectory: dir });
  };
  const seg = $('#segCount');
  seg.addEventListener('input', () => { $('#segLabel').textContent = t('Connections per download: {0}', seg.value); });
  seg.addEventListener('change', () => patch({ segmentCount: parseInt(seg.value, 10) }));
  $('#concMinus').onclick = () => patch({ maxConcurrent: Math.max(1, settings.maxConcurrent - 1) });
  $('#concPlus').onclick = () => patch({ maxConcurrent: Math.min(10, settings.maxConcurrent + 1) });
  syncers.push(() => {
    $('#downloadDir').textContent = settings.downloadDirectory;
    $('#downloadDir').title = settings.downloadDirectory;
    seg.value = settings.segmentCount;
    $('#segLabel').textContent = t('Connections per download: {0}', settings.segmentCount);
    $('#concLabel').textContent = t('Concurrent downloads: {0}', settings.maxConcurrent);
  });

  const auto = $('#autostart');
  invoke('get_autostart').then((on) => { auto.checked = on; }).catch(() => {});
  auto.onchange = async () => {
    try {
      auto.checked = await invoke('set_autostart', { enabled: auto.checked });
      $('#autostartError').classList.add('hidden');
    } catch (e) {
      $('#autostartError').textContent = String(e);
      $('#autostartError').classList.remove('hidden');
      auto.checked = !auto.checked;
    }
  };

  $('#curVersion').textContent = t('Current version: {0}', version);
  $('#checkNow').onclick = () => invoke('check_updates');
  invoke('updater_status').then((s) => { updater = s; renderUpdater(); });
}

function renderUpdater() {
  if (!updater) return;
  // The checker is switched off until a release repository is configured.
  $$('.updates-only').forEach((e) => e.classList.toggle('hidden', !updater.enabled));
  $('#checkNow').textContent = updater.checking ? t('Checking…') : t('Check Now');
  $('#checkNow').disabled = updater.checking;
  $('#latestVersion').textContent = updater.latestVersion ? t('Latest release: {0}', updater.latestVersion) : '';
  $('#lastCheck').textContent = updater.lastCheck ? t('Last checked: {0}', Fmt.dateTime(updater.lastCheck)) : '';
  $('#updateError').textContent = updater.lastError || '';
}

// ---------- File types (IDM style) ----------
function setupFileTypes(b) {
  syncers.push(buildListEditor($('#captureTypes'), {
    key: 'captureTypes',
    label: t('Automatically start downloading the following file types:'),
    hint: t('Separate types with spaces; * and ? are allowed (R0*).'),
    rows: 5,
    defaultValue: b.defaultCaptureTypes,
    getSettings: () => settings,
  }));
  syncers.push(buildListEditor($('#excludedSites'), {
    key: 'excludedSites',
    label: t("Don't start downloading automatically from the following sites:"),
    hint: t('(separate names by spaces)'),
    rows: 3,
    defaultValue: b.defaultExcludedSites,
    getSettings: () => settings,
  }));
  const dlg = $('#addressDialog');
  $('#editAddresses').onclick = () => {
    $('#addressText').value = settings.excludedAddresses.join('\n');
    dlg.showModal();
    $('#addressText').focus();
  };
  $('#addressCancel').onclick = () => dlg.close();
  $('#addressSave').onclick = async () => {
    const list = $('#addressText').value.split(/\r?\n/).map((s) => s.trim()).filter(Boolean);
    await patch({ excludedAddresses: list });
    dlg.close();
  };
  syncers.push(() => {
    const n = settings.excludedAddresses.length;
    $('#addressCount').textContent = n ? t('{0} addresses', n) : '';
  });
}

// ---------- Browser extensions ----------
let browsers = [];

function setupBrowser() {
  $('#openWizard').innerHTML = icon('wand') + `<span>${t('Open Setup Wizard…')}</span>`;
  $('#openWizard').onclick = () => invoke('show_wizard');
  $('#refreshBrowsers').onclick = refreshBrowsers;
  $('#showPlugins').onclick = () => invoke('open_folder', { which: 'plugins' });
  const port = $('#port');
  port.addEventListener('change', () => {
    const v = parseInt(port.value, 10);
    if (v >= 1024 && v <= 65535) patch({ serverPort: v }); else port.value = settings.serverPort;
  });
  syncers.push(() => { if (document.activeElement !== port) port.value = settings.serverPort; });
  renderBridge();
  // Keep the status live: the wizard and the extensions change it while this page is open.
  setInterval(() => { if ($('#page-browser').classList.contains('active')) refreshBrowsers(); }, 3000);
}

async function refreshBrowsers() {
  browsers = await invoke('browsers_status');
  server = await invoke('server_status');
  renderBrowsers();
  renderBridge();
}

function connected(key, withinMs = 3600 * 1000) {
  const at = server && server.connections && server.connections[key];
  return !!at && Date.now() - at < withinMs;
}

function renderBrowsers() {
  const list = $('#browserList');
  list.innerHTML = '';
  for (const b of browsers) {
    let status;
    if (!b.installed) status = el('span', { class: 'browser-status muted', text: t('Browser not found') });
    else if (connected(b.key)) status = el('span', { class: 'browser-status success', html: icon('check') + `<span>${t('Connected')}</span>` });
    else if (b.filesPrepared) status = el('span', { class: 'browser-status muted', text: t('Files prepared, not loaded yet') });
    else status = el('span', { class: 'browser-status muted', text: t('Extension not installed') });
    list.append(el('div', { class: 'item' }, el('span', { html: icon('globe') }), el('div', { class: 'grow', text: b.name }), status));
  }
  if (browsers.length) $('#pluginsRoot').textContent = browsers[0].folder.replace(/[\\/][^\\/]*$/, '');
}

function renderBridge() {
  if (!server) return;
  $('#bridgeDot').classList.toggle('on', server.running);
  $('#bridgeText').textContent = server.statusText;
}

// ---------- Video ----------
function setupVideo() {
  $('#loginOpen').onclick = () => invoke('login_open');
  $('#loginSave').onclick = () => invoke('login_save').catch((e) => { $('#loginDetail').textContent = String(e); });
  $('#loginClear').onclick = () => invoke('login_clear');
  $('#installYtdlp').onclick = () => invoke('install_tool', { tool: 'ytdlp' }).catch(() => {});
  $('#installDeno').onclick = () => invoke('install_tool', { tool: 'deno' }).catch(() => {});
  const custom = $('#ytdlpCustom');
  custom.addEventListener('change', () => patch({ ytdlpPath: custom.value.trim() }).then(() => invoke('tools_refresh')));
  $('#chooseYtdlp').onclick = async () => {
    const file = await TAURI.dialog.open({ multiple: false, filters: [{ name: 'yt-dlp', extensions: ['exe'] }] });
    if (file) { await patch({ ytdlpPath: file }); invoke('tools_refresh'); }
  };
  syncers.push(() => {
    if (document.activeElement !== custom) custom.value = settings.ytdlpPath;
    invoke('effective_ytdlp_args').then((args) => { $('#effectiveArgs').textContent = t('Effective arguments: {0}', args); });
  });
  invoke('login_status').then((s) => { login = s; renderLogin(); });
  invoke('tools_status').then((s) => { tools = s; renderTools(); });
}

function renderLogin() {
  if (!login) return;
  if (login.cookieCount > 0) {
    $('#loginState').textContent = t('Signed in: {0}', login.domains.slice(0, 4).join(', '));
    $('#loginDetail').textContent = login.savedAt ? t('{0} cookies saved {1}', login.cookieCount, Fmt.dateTime(login.savedAt)) : '';
  } else {
    $('#loginState').textContent = t('Not signed in');
    $('#loginDetail').textContent = '';
  }
  $('#loginSave').classList.toggle('hidden', !login.windowOpen);
  $('#loginClear').disabled = login.cookieCount === 0;
}

// "deno 2.9.7 (stable, …)" → "2.9.7"; numeric comparison of dotted versions (updater::is_newer).
const versionOf = (v) => String(v || '').replace(/^deno /, '').split(/\s/)[0].replace(/^v/, '');
function isNewer(a, b) {
  const x = a.split('.').map((n) => parseInt(n, 10) || 0), y = b.split('.').map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
  }
  return false;
}

/** Install, "Up to date" (nothing to do), "Update to <version>", or a plain update check when GitHub was not reached. */
function toolButton(key, btn, path, installed, latest) {
  const label = (name) => t(key === 'ytdlp' ? `${name} yt-dlp` : `${name} deno`);
  if (tools.busy && tools.active === key) { btn.textContent = t('Installing…'); btn.disabled = true; return; }
  btn.disabled = tools.busy;
  if (!path) { btn.textContent = label('Install'); return; }
  if (latest && installed && !isNewer(latest, versionOf(installed))) {
    btn.textContent = t('Up to date');
    btn.disabled = true;
    return;
  }
  btn.textContent = latest ? t('Update to {0}', latest) : label('Update');
}

function renderTools() {
  if (!tools) return;
  $('#ytdlpName').textContent = tools.ytdlpPath ? (tools.ytdlpVersion ? `yt-dlp ${tools.ytdlpVersion}` : 'yt-dlp') : t('yt-dlp not found');
  $('#ytdlpName').classList.toggle('danger', !tools.ytdlpPath);
  $('#ytdlpPath').textContent = tools.ytdlpPath || '';
  $('#denoName').textContent = tools.denoPath ? (tools.denoVersion || 'deno') : t('deno not installed (needed for YouTube)');
  $('#denoName').classList.toggle('warning', !tools.denoPath);
  $('#denoPath').textContent = tools.denoPath || '';
  toolButton('ytdlp', $('#installYtdlp'), tools.ytdlpPath, tools.ytdlpVersion, tools.ytdlpLatest);
  toolButton('deno', $('#installDeno'), tools.denoPath, tools.denoVersion, tools.denoLatest);
  // Progress and messages appear under the tool they belong to.
  for (const key of ['ytdlp', 'deno']) {
    const mine = tools.active === key;
    $(`#${key}ProgressRow`).classList.toggle('hidden', !(mine && tools.busy));
    $(`#${key}Progress`).style.width = Math.round((tools.progress || 0) * 100) + '%';
    $(`#${key}MessageRow`).classList.toggle('hidden', !(mine && tools.message));
    $(`#${key}Message`).textContent = mine ? tools.message || '' : '';
  }
  $('#ffmpegLine').textContent = tools.ffmpegPath ? t('ffmpeg: {0}', tools.ffmpegPath) : t('ffmpeg not found (merging video and audio will not work)');
  $('#ffmpegLine').classList.toggle('danger', !tools.ffmpegPath);
}

init();
