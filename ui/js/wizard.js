// Guided, one-browser-at-a-time setup of the browser extension (Chrome, Edge, Brave: loaded unpacked from a folder).
'use strict';

const STEPS = 4;
let browsers = [];
let index = 0;
let step = 0;
let server = null;
let error = '';
let copied = false;

const closeWindow = () => TAURI.window.getCurrentWindow().close();
const browser = () => browsers[index];
const isLastStep = () => step >= STEPS - 1;
const isLastBrowser = () => index >= browsers.length - 1;

async function init() {
  const b = await invoke('bootstrap');
  await initI18n(b.lang);
  server = b.server;
  browsers = (await invoke('browsers_status')).filter((x) => x.installed);
  $('#bicon').innerHTML = icon('puzzle');
  $('#back').onclick = back;
  $('#skip').onclick = skip;
  $('#next').onclick = next;
  listen('server', (s) => { server = s; if (step === STEPS - 1) render(); });
  // The extension connects right after it is loaded; keep the status row live.
  setInterval(async () => { server = await invoke('server_status'); if (step === STEPS - 1) render(); }, 2000);
  render();
}

function connected(key) {
  const at = server && server.connections && server.connections[key];
  return !!at && Date.now() - at < 120 * 1000;
}

function next() {
  error = ''; copied = false;
  if (isLastStep()) {
    if (isLastBrowser()) return closeWindow();
    index++; step = 0;
  } else {
    step++;
  }
  render();
}

function back() {
  error = '';
  if (step > 0) step--; else if (index > 0) { index--; step = 0; }
  render();
}

function skip() {
  error = '';
  if (isLastBrowser()) return closeWindow();
  index++; step = 0;
  render();
}

const title = (text) => el('h3', { text });
const para = (text) => el('p', { text });
const pathBox = (text) => el('div', { class: 'path-box', text });
const statusRow = (ok, okText, pendingText) =>
  el('div', { class: 'status ' + (ok ? 'ok' : 'pending'), html: icon(ok ? 'check' : 'clock') + `<span>${ok ? okText : pendingText}</span>` });
function button(text, ic, onclick, cls = 'btn') {
  return el('button', { class: cls, html: (ic ? icon(ic) : '') + `<span>${text}</span>`, onclick });
}

function render() {
  const main = $('#step');
  main.innerHTML = '';
  if (!browsers.length) {
    $('#browserName').textContent = t('Browser Extension Setup');
    $('#progressText').textContent = '';
    main.append(para(t('No supported browser was found (Chrome, Edge, Brave).')));
    $('#back').classList.add('hidden');
    $('#skip').classList.add('hidden');
    $('#next').textContent = t('Close');
    $('#next').onclick = closeWindow;
    return;
  }
  const b = browser();
  $('#browserName').textContent = b.name;
  $('#progressText').textContent = t('Browser {0} of {1} · Step {2} of {3}', index + 1, browsers.length, step + 1, STEPS);
  $('#dots').innerHTML = Array.from({ length: STEPS }, (_, i) => `<span class="${i <= step ? 'on' : ''}"></span>`).join('');
  $('#back').disabled = step === 0 && index === 0;
  $('#skip').classList.toggle('hidden', isLastBrowser() && isLastStep());
  $('#next').textContent = isLastStep() ? (isLastBrowser() ? t('Finish') : t('Next Browser')) : t('Next');

  switch (step) {
    case 0: {
      main.append(
        title(t('Prepare the extension files')),
        para(t('{0} does not allow apps to install extensions automatically, so the extension is loaded from a folder. The files are placed in:', b.name)),
        pathBox(b.folder),
        el('div', { class: 'row' },
          button(t('Prepare Files'), 'folder', async () => {
            try { await invoke('prepare_browser_files', { key: b.key }); error = ''; b.filesPrepared = true; }
            catch (e) { error = String(e); }
            render();
          }, 'btn primary'),
          button(t('Show in Explorer'), null, () => invoke('open_folder', { which: 'browser:' + b.key }))),
        statusRow(b.filesPrepared, t('Files are ready'), t('Click “Prepare Files”')),
        el('p', { class: 'caption', text: t('Keep this folder in place; the browser loads the extension from it.') }));
      break;
    }
    case 1: {
      // Edge does not let apps open edge:// pages: it opens a new window and the address is pasted by hand.
      const manual = b.key === 'edge';
      main.append(...[
        title(t('Open the extensions page and turn on Developer mode')),
        para(manual
          ? t('Click the button below: Edge opens a new window and the address below is copied. Paste it into the address bar (Ctrl+V) and press Enter. Then switch on “Developer mode” in the left sidebar.')
          : t('Click the button below. On the page that opens, switch on “Developer mode” (top right in Chrome and Brave, in the left sidebar in Edge).')),
        el('div', { class: 'row' }, button(manual ? t('Open Edge and copy the address') : t('Open {0}', b.extensionsPage), 'globe', async () => {
          try { b.pasteHint = await invoke('open_extensions_page', { key: b.key }); error = ''; } catch (e) { error = String(e); }
          render();
        }, 'btn primary')),
        manual ? pathBox(b.extensionsPage) : null,
        b.pasteHint ? statusRow(true, t('Address copied: paste it into the address bar of the new Edge window (Ctrl+V) and press Enter.'), '') : null,
      ].filter(Boolean));
      break;
    }
    case 2:
      main.append(
        title(t('Load the extension folder')),
        para(t('Click “Load unpacked” on the extensions page. In the folder dialog, paste the path below into the address bar (Ctrl+L), press Enter, then click “Select Folder”.')),
        pathBox(b.folder),
        el('div', { class: 'row' },
          button(copied ? t('Copied') : t('Copy Folder Path'), 'copy', async () => { await invoke('copy_text', { text: b.folder }); copied = true; render(); }, 'btn primary'),
          button(t('Show in Explorer'), null, () => invoke('open_folder', { which: 'browser:' + b.key }))));
      break;
    default:
      main.append(
        title(t('{0} is ready', b.name)),
        para(t('Pin EDM to the toolbar (puzzle icon ▸ pin). Its popup should show “Connected”. Downloads of the captured file types are now handed to the app.')),
        statusRow(connected(b.key), t('Extension has connected to the app'), t('Waiting for the extension to connect (it connects right after loading)')));
  }
  if (error) main.append(el('p', { class: 'caption danger', text: error }));
}

init();
