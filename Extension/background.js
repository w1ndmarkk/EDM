// EDM bridge extension — background script (Chrome, Edge, Brave; MV3)
'use strict';

importScripts('hls.js');

const api = typeof browser !== 'undefined' ? browser : chrome;
const t = (k, subs) => { try { return api.i18n.getMessage(k, subs) || k; } catch { return k; } };

const DEFAULTS = { port: 48213, intercept: true, minSizeMB: 0, sniff: true, panel: true };
const BROWSER = navigator.brave ? 'brave' : (/Edg\//.test(navigator.userAgent) ? 'edge' : 'chrome');

const MEDIA_EXT = /\.(mp4|m4v|webm|mkv|mov|avi|flv|mp3|m4a|aac|ogg|opus|wav|flac)(\?|#|$)/i;
const FRAGMENT = /\.(ts|m4s|m4f|cmfv|cmfa|fmp4|init)(\?|#|$)|[/_.-](seg|segment|chunk|frag|fragment)[-_]?\d+/i;
const IGNORED_HOSTS = /(^|\.)googlevideo\.com$|(^|\.)fbcdn\.net$|(^|\.)twimg\.com$/i;

// ---------- Settings ----------
async function getSettings() {
  const s = await api.storage.local.get('settings');
  return { ...DEFAULTS, ...(s.settings || {}) };
}
async function setSettings(patch) {
  const cur = await getSettings();
  await api.storage.local.set({ settings: { ...cur, ...patch } });
}
const base = (s) => `http://127.0.0.1:${s.port}`;
// Custom header = proof the request comes from an extension: ordinary web pages cannot send it
// without a CORS preflight, and the bridge only grants CORS to extension origins. Chrome omits the
// Origin header on extension GET requests that are covered by host permissions, so Origin alone is not enough.
const BRIDGE_HEADERS = { 'X-EDM-Extension': BROWSER };

// ---------- App communication ----------
async function ping(settings) {
  settings = settings || (await getSettings());
  try {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 1200);
    const r = await fetch(base(settings) + '/ping?browser=' + BROWSER, { signal: ctrl.signal, cache: 'no-store', headers: BRIDGE_HEADERS });
    clearTimeout(t);
    if (!r.ok) return null;
    const app = await r.json();
    if (app && app.active > 0) startProgress();
    return app;
  } catch {
    return null;
  }
}

// ---------- Download progress: toolbar badge + popup list ----------
async function bridgeJSON(path, init) {
  const settings = await getSettings();
  const r = await fetch(base(settings) + path, { cache: 'no-store', ...init, headers: { ...BRIDGE_HEADERS, ...(init?.headers || {}) } });
  return r.json();
}
async function getDownloads() {
  try { const j = await bridgeJSON('/downloads'); return (j && j.items) || []; } catch { return []; }
}
async function downloadAction(id, action) {
  try {
    const j = await bridgeJSON('/action', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id, action }) });
    tickProgress();
    return j;
  } catch (e) { return { ok: false, error: String(e) }; }
}
const ACTIVE_STATES = new Set(['downloading', 'probing']);
let progressTimer = null;
let badgeShown = false;
async function setGlobalBadge(text, color) {
  try { await api.action.setBadgeText({ text }); if (color) await api.action.setBadgeBackgroundColor({ color }); } catch {}
}
// Per-tab media counts override the global badge; while downloads run, every tab falls back to the global text.
async function useGlobalBadgeOnAllTabs(on) {
  try {
    const tabs = await api.tabs.query({});
    for (const tab of tabs) {
      if (on) { try { await api.action.setBadgeText({ tabId: tab.id, text: null }); } catch {} }
      else { const list = await readMedia(tab.id); try { await api.action.setBadgeText({ tabId: tab.id, text: list.length ? String(list.length) : '' }); } catch {} }
    }
  } catch {}
}
async function tickProgress() {
  const list = await getDownloads();
  const active = list.filter((i) => ACTIVE_STATES.has(i.state));
  if (active.length) {
    const measurable = active.filter((i) => i.total > 0);
    let text = '…';
    if (active.length > 1) text = active.length + '↓';
    else if (measurable.length) {
      const total = measurable.reduce((a, i) => a + i.total, 0);
      const got = measurable.reduce((a, i) => a + Math.min(i.received, i.total), 0);
      text = Math.round((100 * got) / total) + '%';
    }
    if (!badgeShown) { badgeShown = true; await useGlobalBadgeOnAllTabs(true); }
    await setGlobalBadge(text, '#2f6feb');
  } else {
    stopProgress();
    if (badgeShown) {
      badgeShown = false;
      await setGlobalBadge('✓', '#2ebd59');
      setTimeout(async () => { await setGlobalBadge('', '#2f6feb'); await useGlobalBadgeOnAllTabs(false); }, 4000);
    }
  }
}
function startProgress() {
  if (progressTimer) return;
  tickProgress();
  progressTimer = setInterval(tickProgress, 1500);  // each fetch keeps the MV3 worker alive while something runs
}
function stopProgress() {
  if (progressTimer) { clearInterval(progressTimer); progressTimer = null; }
}

async function cookiesFor(url) {
  try {
    const list = await api.cookies.getAll({ url });
    return {
      header: list.map((c) => `${c.name}=${c.value}`).join('; '),
      list: list.map((c) => ({
        name: c.name, value: c.value, domain: c.domain, path: c.path,
        secure: !!c.secure, expirationDate: c.expirationDate || 0,
      })),
    };
  } catch {
    return { header: '', list: [] };
  }
}

function notify(title, message) {
  try {
    api.notifications?.create?.('', {
      type: 'basic', iconUrl: 'icons/icon128.png', title, message: String(message || '').slice(0, 200),
    });
  } catch {}
}

// No notification when a download starts: the app's download window is the confirmation. Only a failure to reach
// the app is reported, and not for the in-page panel, which says so itself.
async function sendToApp({ url, filename, referer, kind, format, audio, title, manifest, quiet }) {
  const settings = await getSettings();
  // A blob: playlist travels as text; its segments live on the page's server (and so do its cookies).
  const cookieUrl = /^https?:/i.test(url) ? url : referer;
  const ck = cookieUrl ? await cookiesFor(cookieUrl) : { header: '', list: [] };
  const payload = {
    url, filename: filename || '', referer: referer || '', kind: kind || 'file',
    userAgent: navigator.userAgent, cookieHeader: ck.header, cookies: ck.list,
    format: format || null, audio: audio || null, title: title || null, manifest: manifest || null,
  };
  try {
    const r = await fetch(base(settings) + '/download', {
      method: 'POST', headers: { 'Content-Type': 'application/json', ...BRIDGE_HEADERS }, body: JSON.stringify(payload),
    });
    const j = await r.json();
    if (!j.ok) throw new Error(j.error || 'app error');
    startProgress();
    return { ok: true };
  } catch (e) {
    if (!quiet) notify(t('notifOfflineTitle'), t('notifOfflineBody'));
    return { ok: false, error: String(e) };
  }
}

/** Opens the app's playlist dialog with this address (YouTube playlists and channel video pages). */
async function openPlaylistInApp(url) {
  try {
    const j = await bridgeJSON('/playlist', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ url }) });
    return { ok: !!(j && j.ok) };
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

// ---------- Download interception ----------
// The app decides (Settings ▸ File Types: file types, excluded sites, excluded addresses). Any failure — the app
// is not running, an old version — leaves the download to the browser.
async function shouldCapture(url, filename) {
  try {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 1500);
    const j = await bridgeJSON('/check', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ url, filename }), signal: ctrl.signal,
    });
    clearTimeout(timer);
    return !!(j && j.ok && j.capture);
  } catch {
    return false;
  }
}

async function interceptDownload(item) {
  const settings = await getSettings();
  if (!settings.intercept) return false;
  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url)) return false;
  if (item.byExtensionId) return false;
  if (settings.minSizeMB > 0 && item.totalBytes > 0 && item.totalBytes < settings.minSizeMB * 1024 * 1024) return false;
  const filename = item.filename ? item.filename.split(/[\\/]/).pop() : '';
  if (!(await shouldCapture(url, filename))) return false;

  try { await api.downloads.cancel(item.id); } catch {}
  try { await api.downloads.erase({ id: item.id }); } catch {}
  sendToApp({ url, filename, referer: item.referrer, kind: 'file' });
  return true;
}

api.downloads.onDeterminingFilename.addListener((item, suggest) => {
  interceptDownload(item).then((took) => { if (!took) { try { suggest(); } catch {} } });
  return true;
});

// ---------- Formats for the in-page "Download this video" panel ----------
// Read as soon as a player appears (like IDM), so the list is ready when the panel is clicked. One request per page;
// the app keeps its own cache too, for when this worker has been asleep.
const formatsCache = new Map(); // url -> { time, promise }
const FORMATS_TTL = 30 * 60 * 1000;
function cachedFormats(url, referer) {
  const hit = formatsCache.get(url);
  if (hit && Date.now() - hit.time < FORMATS_TTL) return hit.promise;
  const promise = videoFormats(url, referer).then((r) => {
    if (!r || !r.ok) formatsCache.delete(url); // offline or failed: ask again next time
    return r;
  });
  formatsCache.set(url, { time: Date.now(), promise });
  while (formatsCache.size > 40) formatsCache.delete(formatsCache.keys().next().value);
  return promise;
}

async function videoFormats(url, referer) {
  const ck = await cookiesFor(url);
  try {
    return await bridgeJSON('/formats', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, referer: referer || url, userAgent: navigator.userAgent, cookies: ck.list }),
    });
  } catch (e) {
    // "Not running" only when the app really does not answer; otherwise the request itself failed.
    return { ok: false, offline: !(await ping()), error: String(e) };
  }
}

// ---------- Media sniffing ----------
const mem = new Map(); // tabId -> Map(url -> info)   (fallback when storage.session is unavailable)
// Media list updates of one tab run one after another: concurrent read-modify-writes used to drop entries.
const tabQueues = new Map();
function withTab(tabId, fn) {
  const next = (tabQueues.get(tabId) || Promise.resolve()).then(fn, fn);
  tabQueues.set(tabId, next.catch(() => {}));
  return next;
}

async function readMedia(tabId) {
  if (api.storage.session) {
    const key = 'media:' + tabId;
    const r = await api.storage.session.get(key);
    return r[key] || [];
  }
  return Array.from((mem.get(tabId) || new Map()).values());
}
async function writeMedia(tabId, list) {
  if (api.storage.session) {
    await api.storage.session.set({ ['media:' + tabId]: list });
  } else {
    mem.set(tabId, new Map(list.map((m) => [m.url, m])));
  }
  try {
    await api.action.setBadgeText({ tabId, text: list.length ? String(list.length) : '' });
    await api.action.setBadgeBackgroundColor({ tabId, color: '#2f6feb' });
  } catch {}
}
async function clearMedia(tabId) {
  probed.delete(tabId);
  await withTab(tabId, async () => {
    if (api.storage.session) await api.storage.session.remove('media:' + tabId);
    mem.delete(tabId);
  });
  try { await api.action.setBadgeText({ tabId, text: '' }); } catch {}
}

function normalizeKey(url) {
  try {
    const u = new URL(url);
    ['range', 'bytes', 'start', 'end', 'rn', 'rbuf', 'r', '_'].forEach((p) => u.searchParams.delete(p));
    return u.origin + u.pathname + (u.searchParams.toString() ? '?' + u.searchParams.toString() : '');
  } catch {
    return url;
  }
}

async function addMedia(tabId, info) {
  if (tabId < 0) return;
  const settings = await getSettings();
  if (!settings.sniff) return;
  await withTab(tabId, async () => {
    const list = await readMedia(tabId);
    const key = normalizeKey(info.url);
    const idx = list.findIndex((m) => m.key === key);
    const entry = { ...info, key, name: info.name || guessName(info.url, info.type) };
    if (idx >= 0) {
      const old = list[idx];
      const better = (info.size || 0) > (old.size || 0) || (info.details && !old.details) || (info.manifest && !old.manifest);
      if (better) {
        // Keep what the earlier sighting knew (the network saw the size, the page hook read the playlist).
        list[idx] = { ...old, ...entry, details: entry.details || old.details, manifest: entry.manifest || old.manifest,
          size: Math.max(info.size || 0, old.size || 0) };
        await writeMedia(tabId, list);
      }
      return;
    }
    list.unshift(entry);
    // Streams and subtitles are what the panel is for: drop plain media files first when the list is full.
    while (list.length > 60) {
      const i = list.map((m) => m.kind).lastIndexOf('media');
      list.splice(i >= 0 ? i : list.length - 1, 1);
    }
    await writeMedia(tabId, list);
  });
  // A new stream: read its resolutions and duration right away, so the panel opens with them.
  if (info.kind === 'hls' && !info.details) scheduleDescribe(tabId);
  // Audio without a visible player gets a panel docked at the bottom of the page.
  if ((info.kind === 'media' && isAudio(info)) || (info.kind === 'hls' && isAudioOnlyHls(info.details))) announceAudio(tabId);
}

const AUDIO_EXT = /\.(mp3|m4a|aac|ogg|oga|opus|wav|flac|weba|wma)(\?|#|$)/i;
const isAudio = (m) => /^audio\//i.test(m.type || '') || (!/^video\//i.test(m.type || '') && AUDIO_EXT.test(m.url));
/** An HLS stream with only audio renditions (no resolution, no video codec). */
const isAudioOnlyHls = (d) => !!(d && d.variants && d.variants.length
  && d.variants.every((v) => !v.height && !/avc|hvc|hev|vp0?8|vp0?9|av01/i.test(v.codecs || '')));
function announceAudio(tabId) {
  try { api.tabs.sendMessage(tabId, { type: 'audioCaptured' }, { frameId: 0 }).catch(() => {}); } catch {}
}

const describeTimers = new Map();
function scheduleDescribe(tabId) {
  clearTimeout(describeTimers.get(tabId));
  describeTimers.set(tabId, setTimeout(() => { describeTimers.delete(tabId); describeTabMedia(tabId).catch(() => {}); }, 400));
}

// One reading per tab at a time: a later call waits for the running one, then picks up only what is still missing.
const describing = new Map(); // tabId -> promise
function describeTabMedia(tabId) {
  const next = (describing.get(tabId) || Promise.resolve()).catch(() => {}).then(() => describeTabMediaNow(tabId));
  describing.set(tabId, next);
  next.finally(() => { if (describing.get(tabId) === next) describing.delete(tabId); }).catch(() => {});
  return next;
}

/** Fills in HLS details (resolutions, duration) for the panel, reading the playlist in the frame that loaded it. */
async function describeTabMediaNow(tabId) {
  const list = await readMedia(tabId);
  const todo = list.filter((m) => m.kind === 'hls' && m.details === undefined).slice(0, 8);
  if (!todo.length) return list;
  const timeout = (ms) => new Promise((r) => setTimeout(() => r(null), ms));
  const found = await Promise.all(todo.map(async (m) => {
    let d = null;
    try { d = await Promise.race([api.tabs.sendMessage(tabId, { type: 'parseHls', url: m.url }, { frameId: Math.max(0, m.frameId || 0) }), timeout(6000)]); } catch {}
    if (!d) { try { d = await Promise.race([EDM_HLS.describe(m.url), timeout(6000)]); } catch {} }
    return [m.key, d || null];
  }));
  return withTab(tabId, async () => {
    const current = await readMedia(tabId);
    for (const [key, d] of found) { const m = current.find((x) => x.key === key); if (m) m.details = d; }
    if (found.some(([, d]) => isAudioOnlyHls(d))) announceAudio(tabId);
    if (api.storage.session) await api.storage.session.set({ ['media:' + tabId]: current });
    else mem.set(tabId, new Map(current.map((m) => [m.url, m])));
    return current;
  });
}

// Responses whose type gives nothing away (text/plain, octet-stream) can still be HLS playlists:
// the frame that requested them reads their first bytes. Bounded per tab.
const probed = new Map(); // tabId -> Set(keys)
async function probePlaylist(d) {
  const set = probed.get(d.tabId) || new Set();
  probed.set(d.tabId, set);
  const key = normalizeKey(d.url);
  if (set.size >= 40 || set.has(key)) return;
  set.add(key);
  let isPlaylist = false;
  try {
    const r = await api.tabs.sendMessage(d.tabId, { type: 'probePlaylist', url: d.url }, { frameId: Math.max(0, d.frameId) });
    isPlaylist = !!(r && r.playlist);
  } catch {
    try { isPlaylist = await EDM_HLS.looksLikePlaylist(d.url); } catch {}
  }
  if (isPlaylist) addMedia(d.tabId, { url: d.url, type: 'application/vnd.apple.mpegurl', size: 0, kind: 'hls', frameId: d.frameId, time: Date.now() });
}

function guessName(url, type) {
  try {
    const u = new URL(url);
    let n = decodeURIComponent(u.pathname.split('/').filter(Boolean).pop() || '') || u.hostname;
    if (!/\.[a-z0-9]{2,5}$/i.test(n) && type) {
      const ext = { 'video/mp4': 'mp4', 'video/webm': 'webm', 'audio/mpeg': 'mp3', 'audio/mp4': 'm4a', 'video/quicktime': 'mov' }[type.toLowerCase()];
      if (ext) n += '.' + ext;
    }
    return n;
  } catch {
    return url;
  }
}

function headerVal(headers, name) {
  const h = (headers || []).find((x) => x.name.toLowerCase() === name);
  return h ? h.value : '';
}
const SUBTITLE_EXT = /\.(srt|vtt|webvtt|ass|ssa|ttml|dfxp)(\?|#|$)/i;
const SUBTITLE_CT = /text\/vtt|x-subrip|application\/x-srt|text\/srt|ttml\+xml/;
// Segmented subtitles of an HLS stream (sub_00012.vtt, seg-3.vtt) are pieces, not files.
const SUBTITLE_PIECE = /(seg|segment|chunk|frag|part)[-_]?\d+[^/]*$|[-_]\d{3,}\.(vtt|webvtt)(\?|#|$)/i;
const TS_FILE = /\.ts(\?|#|$)/i;
const PLAYLIST_CANDIDATE_CT = /^(text\/plain|application\/octet-stream|binary\/octet-stream|application\/x-unknown|)$/;
const NOT_PLAYLIST = /\.(js|mjs|css|json|png|jpe?g|gif|webp|avif|svg|ico|woff2?|ttf|html?|xml|wasm|mp4|m4s|m4a|ts|aac|mp3|webm|key)(\?|#|$)/i;

function classify(url, ct, size) {
  ct = (ct || '').toLowerCase();
  if (/\.m3u8(\?|#|$)/i.test(url) || /mpegurl/.test(ct)) return 'hls';
  if (/\.mpd(\?|#|$)/i.test(url) || /dash\+xml/.test(ct)) return 'dash';
  if (SUBTITLE_EXT.test(url) || SUBTITLE_CT.test(ct)) return SUBTITLE_PIECE.test(url) ? null : 'subtitle';
  // A whole MPEG-TS file is a video; the small ones are HLS segments.
  if (TS_FILE.test(url) || /mp2t/.test(ct)) return size >= 20 * 1024 * 1024 ? 'media' : null;
  if (FRAGMENT.test(url)) return null;
  if (/^(video|audio)\//.test(ct)) {
    if (/iso\.segment|x-mpegurl/.test(ct)) return null;
    return 'media';
  }
  if (/octet-stream/.test(ct) && MEDIA_EXT.test(url)) return 'media';
  return null;
}

if (api.webRequest?.onHeadersReceived) {
  api.webRequest.onHeadersReceived.addListener(
    (d) => {
      if (d.tabId < 0 || d.statusCode >= 400) return;
      // A new page in the tab: forget the old page's media. (Not on tabs.onUpdated: sites that change their address
      // with history.pushState — most streaming sites — would lose what the player loaded.)
      if (d.type === 'main_frame' && d.statusCode < 300 && /html/i.test(headerVal(d.responseHeaders, 'content-type'))) {
        clearMedia(d.tabId);
        return;
      }
      let host = '';
      try { host = new URL(d.url).hostname; } catch {}
      if (IGNORED_HOSTS.test(host)) return;
      const ct = headerVal(d.responseHeaders, 'content-type');
      let size = parseInt(headerVal(d.responseHeaders, 'content-length'), 10) || 0;
      const cr = headerVal(d.responseHeaders, 'content-range').match(/\/(\d+)/);
      if (cr) size = parseInt(cr[1], 10);
      // Internet radio (Icecast/SHOUTcast) never ends: not a file to download.
      if (headerVal(d.responseHeaders, 'icy-name') || headerVal(d.responseHeaders, 'icy-br') || headerVal(d.responseHeaders, 'icy-metaint')) return;
      const kind = classify(d.url, ct, size);
      if (!kind) {
        const base = ct.split(';')[0].trim().toLowerCase();
        const candidate = (d.type === 'xmlhttprequest' || d.type === 'other') && PLAYLIST_CANDIDATE_CT.test(base)
          && (size === 0 || size < 300 * 1024) && !NOT_PLAYLIST.test(d.url) && !FRAGMENT.test(d.url);
        if (candidate) probePlaylist(d);
        return;
      }
      if (kind === 'media' && size > 0 && size < 300 * 1024) return;
      addMedia(d.tabId, { url: d.url, type: ct.split(';')[0], size, kind, frameId: d.frameId, time: Date.now() });
    },
    { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'other', 'object', 'main_frame', 'sub_frame'] },
    ['responseHeaders']
  );
}

api.tabs.onRemoved.addListener((tabId) => clearMedia(tabId));

// ---------- Context menu ----------
const menus = api.contextMenus || api.menus;
function createMenus() {
  if (!menus) return;
  try {
    menus.removeAll(() => {
      menus.create({ id: 'dm-link', title: t('menuDownloadLink'), contexts: ['link', 'video', 'audio', 'image'] });
      menus.create({ id: 'dm-page-video', title: t('menuPageVideo'), contexts: ['page', 'video', 'frame', 'action'] });
      menus.create({
        id: 'dm-playlist', title: t('menuPlaylist'), contexts: ['page', 'link', 'video'],
        documentUrlPatterns: ['*://*.youtube.com/*'],
      });
    });
  } catch {}
}
api.runtime.onInstalled.addListener(() => { createMenus(); ping(); });
api.runtime.onStartup?.addListener(() => { createMenus(); ping(); });
menus?.onClicked.addListener((info, tab) => {
  if (info.menuItemId === 'dm-link') {
    const url = info.linkUrl || info.srcUrl;
    if (url) sendToApp({ url, referer: info.pageUrl || tab?.url, kind: 'file' });
  } else if (info.menuItemId === 'dm-playlist') {
    // A playlist link that was right-clicked, else the page itself.
    const link = info.linkUrl && /[?&]list=|\/playlist\b/.test(info.linkUrl) ? info.linkUrl : '';
    openPlaylistInApp(link || info.pageUrl || tab?.url).then((r) => { if (!r.ok) notify(t('notifOfflineTitle'), t('notifOfflineBody')); });
  } else if (info.menuItemId === 'dm-page-video') {
    const url = info.pageUrl || tab?.url;
    if (url) sendToApp({ url, referer: url, kind: 'video' });
  }
});

// ---------- Popup / content script messages ----------
api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  (async () => {
    try {
      switch (msg?.type) {
        case 'status': {
          const settings = await getSettings();
          const app = await ping(settings);
          sendResponse({ online: !!app, app, settings });
          break;
        }
        case 'panelSettings': sendResponse({ panel: (await getSettings()).panel }); break;
        case 'getMedia': sendResponse(await readMedia(msg.tabId)); break;
        case 'tabMedia': {
          const items = sender.tab ? await describeTabMedia(sender.tab.id) : [];
          sendResponse({ title: (sender.tab && sender.tab.title) || '', items });
          break;
        }
        case 'formats': sendResponse(await cachedFormats(msg.url, msg.referer)); break;
        case 'prefetch': {
          // A player became visible: read its qualities and the captured playlists now, not on click.
          if (typeof msg.url === 'string' && /^https?:/i.test(msg.url)) cachedFormats(msg.url, msg.referer).catch(() => {});
          if (sender.tab) describeTabMedia(sender.tab.id).catch(() => {});
          sendResponse({ ok: true });
          break;
        }
        case 'downloads': sendResponse(await getDownloads()); break;
        case 'action': sendResponse(await downloadAction(msg.id, msg.action)); break;
        case 'download': sendResponse(await sendToApp(msg)); break;
        case 'openPlaylist': sendResponse(await openPlaylistInApp(msg.url)); break;
        case 'setSettings': await setSettings(msg.settings || {}); sendResponse({ ok: true }); break;
        case 'manifest': {
          // An HLS/DASH playlist the page loaded, recognised by its content (page_hook.js) whatever its URL or type.
          if (sender.tab && (msg.kind === 'hls' || msg.kind === 'dash') && typeof msg.url === 'string') {
            const inline = !/^https?:/i.test(msg.url);
            if (!inline || msg.manifest) {
              await addMedia(sender.tab.id, {
                url: msg.url, type: msg.kind === 'hls' ? 'application/vnd.apple.mpegurl' : 'application/dash+xml', size: 0,
                kind: msg.kind, frameId: sender.frameId, time: Date.now(),
                details: msg.details || undefined, manifest: inline ? String(msg.manifest).slice(0, 8 * 1024 * 1024) : undefined,
              });
            }
          }
          sendResponse({ ok: true });
          break;
        }
        case 'pageMedia': {
          if (sender.tab) {
            for (const m of msg.items || []) {
              const kind = classify(m.url, m.type || '');
              if (kind || MEDIA_EXT.test(m.url)) {
                await addMedia(sender.tab.id, { url: m.url, type: m.type || '', size: 0, kind: kind || 'media', frameId: sender.frameId, time: Date.now(), name: m.name });
              }
            }
          }
          sendResponse({ ok: true });
          break;
        }
        default: sendResponse(null);
      }
    } catch (e) {
      sendResponse({ ok: false, error: String(e) });
    }
  })();
  return true;
});
