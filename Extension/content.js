// EDM — content script (runs in every frame; hls.js and rules.js are loaded before it)
// 1) Reports <video>/<audio>/<source>/<track> URLs on the page to the background script.
// 2) Reads HLS playlists for the background script from inside this frame, so the requests carry the player's
//    own cookies, origin and referrer.
// 3) Shows an IDM-style "Download this video" panel over video and audio players. It lists what was captured in
//    the tab (HLS resolutions, video/audio files, subtitles) and, where yt-dlp knows the site, its qualities.
(() => {
  'use strict';
  if (window.__edmContent) return;
  window.__edmContent = true;
  const api = typeof browser !== 'undefined' ? browser : chrome;
  const t = (k, subs) => { try { return api.i18n.getMessage(k, subs) || k; } catch { return k; } };
  // Promise style works in Chrome (MV3) and Firefox; Firefox's `browser` namespace ignores callbacks.
  const send = (msg) => new Promise((resolve) => {
    try { Promise.resolve(api.runtime.sendMessage(msg)).then(resolve, () => resolve(null)); } catch { resolve(null); }
  });

  api.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (msg && msg.type === 'probePlaylist') {
      EDM_HLS.looksLikePlaylist(msg.url).then((ok) => sendResponse({ playlist: ok }), () => sendResponse(null));
      return true;
    }
    if (msg && msg.type === 'parseHls') {
      EDM_HLS.describe(msg.url).then(sendResponse, () => sendResponse(null));
      return true;
    }
    return false;
  });

  // ---------- Download links: taken over before the browser requests them ----------
  // A click on a link to a captured file type (Settings ▸ File Types) goes to EDM without the browser downloading
  // any of it: on a metered connection the browser must not fetch what EDM fetches again. Types the browser shows
  // itself (PDF, video, audio, images) stay with the browser; the video panel offers those.
  let linkRules = null;
  let intercept = true;
  try {
    Promise.resolve(api.storage.local.get(['captureRules', 'settings'])).then((r) => {
      linkRules = r && r.captureRules ? r.captureRules.rules : null;
      intercept = !(r && r.settings && r.settings.intercept === false);
    }, () => {});
    api.storage.onChanged.addListener((c) => {
      if (c.captureRules) linkRules = c.captureRules.newValue ? c.captureRules.newValue.rules : null;
      if (c.settings) intercept = !(c.settings.newValue && c.settings.newValue.intercept === false);
    });
  } catch {}
  if (window === window.top) send({ type: 'syncRules' });

  function onLinkClick(e) {
    if (!intercept || !linkRules || e.defaultPrevented) return;
    if (e.type === 'click' ? e.button !== 0 : e.button !== 1) return;
    const a = e.composedPath().find((n) => n && (n.tagName === 'A' || n.tagName === 'AREA') && n.href);
    if (!a || !/^https?:/i.test(a.href)) return;
    const url = a.href;
    const filename = (a.getAttribute('download') || '').trim();
    const ext = EDM_RULES.extension(filename) || EDM_RULES.urlExtension(url);
    if (!ext || EDM_RULES.shownExtension(ext) || !EDM_RULES.capture(linkRules, url, filename)) return;
    e.preventDefault();
    e.stopPropagation(); // the page's own handler would navigate to it as well
    const newTab = e.button === 1 || e.ctrlKey || e.shiftKey || e.metaKey || /^_blank$/i.test(a.target || '');
    const target = a.target || '';
    send({ type: 'takeLink', url, filename, referer: location.href }).then((r) => {
      if (r && r.taken) return;
      // A page after all (or the extension did not answer): open it the way the click would have.
      if (newTab) window.open(url, '_blank');
      else if (target && !/^_self$/i.test(target)) window.open(url, target);
      else location.assign(url);
    });
  }
  window.addEventListener('click', onLinkClick, true);
  window.addEventListener('auxclick', onLinkClick, true);

  // ---------- Manifests the page loaded (page_hook.js recognises them by content) ----------
  document.addEventListener('edm-manifest-v1', (e) => {
    let d;
    try { d = JSON.parse(e.detail); } catch { return; }
    if (!d || typeof d.url !== 'string' || typeof d.text !== 'string' || !d.text) return;
    const inline = !/^https?:/i.test(d.url);
    let details;
    if (d.kind === 'hls') {
      try { details = EDM_HLS.parse(d.text, inline ? location.href : d.url); } catch { details = null; }
      if (!details) return;
      // A blob: master playlist pointing at more blob: playlists cannot be handed over; its variants are
      // reported on their own when the player loads them.
      if (inline && details.variants.some((v) => !/^https?:/i.test(v.url))) return;
    }
    send({ type: 'manifest', url: d.url, kind: d.kind, details, manifest: inline ? d.text : '' });
  });

  // ---------- Media URLs on the page ----------
  const reported = new Set();
  let shadowMedia = [];   // media elements inside shadow roots (see mediaElements)
  let shadowScanAt = 0;
  function scanMedia() {
    const items = [];
    const inShadow = shadowMedia.flatMap((m) => [m, ...m.querySelectorAll('source, track')]);
    [...document.querySelectorAll('video, audio, source, track'), ...inShadow].forEach((el) => {
      const src = el.currentSrc || el.src || el.getAttribute('src');
      if (!src || !/^https?:/i.test(src) || reported.has(src)) return;
      reported.add(src);
      items.push({ url: src, type: el.getAttribute('type') || '', name: el.tagName === 'TRACK' ? (el.label || el.srclang || '') : '' });
    });
    if (items.length) send({ type: 'pageMedia', items });
  }
  scanMedia();
  document.addEventListener('play', scanMedia, true);
  document.addEventListener('loadedmetadata', scanMedia, true);
  let scans = 0;
  const scanTimer = setInterval(() => { scanMedia(); if (++scans > 20) clearInterval(scanTimer); }, 3000);

  // ---------- "Download this video" panel ----------
  let enabled = true;
  const readEnabled = (s) => { enabled = !s || s.panel !== false; if (!enabled) hidePanel(); };
  try {
    Promise.resolve(api.storage.local.get('settings')).then((r) => readEnabled(r && r.settings), () => {});
    api.storage.onChanged.addListener((changes) => { if (changes.settings) readEnabled(changes.settings.newValue); });
  } catch {}

  const dismissed = new WeakSet();
  let target = null; // { el, isAudio }
  let host = null, ui = null;
  let menuOpen = false;
  let menuToken = 0;
  let pointer = { x: -1, y: -1 };

  const CSS = `
    :host { all: initial; }
    * { box-sizing: border-box; font-family: "Segoe UI", system-ui, sans-serif; }
    .wrap { position: fixed; z-index: 2147483647; display: none; }
    .bar { display: flex; align-items: stretch; gap: 2px; padding: 2px; border-radius: 5px;
           background: linear-gradient(#f3f8fe, #c9dcf3); border: 1px solid #7f9db9; box-shadow: 0 1px 4px rgba(0,0,0,.35);
           opacity: .4; transition: opacity .15s ease; }
    /* See-through over the video until the pointer is on it (or its menu is open, or it reports a result). */
    .wrap:hover .bar, .wrap:has(.menu.open) .bar, .wrap.flash .bar { opacity: 1; }
    button { all: unset; box-sizing: border-box; cursor: pointer; display: inline-flex; align-items: center; justify-content: center; color: #1a3a6b;
             font-size: 12px; font-weight: 600; line-height: 1; border-radius: 3px; }
    .main { gap: 6px; padding: 4px 9px; border: 1px solid #8eb0d8; background: linear-gradient(#ffffff, #dbe8f8); color: #d4380d; }
    .main:hover { background: linear-gradient(#ffffff, #c4dbf7); }
    .arrow { width: 0; height: 0; border-left: 8px solid #1e9e2e; border-top: 5px solid transparent; border-bottom: 5px solid transparent; }
    .small { width: 18px; border: 1px solid #8eb0d8; background: linear-gradient(#ffffff, #dbe8f8); font-size: 11px; }
    .small:hover { background: #c4dbf7; }
    /* The list looks like IDM's menu: "Download all", then numbered "<title>   <TYPE> file, <size/duration>". */
    .menu { position: absolute; right: 0; top: calc(100% + 3px); width: min(560px, calc(100vw - 16px)); max-height: 380px;
            overflow-x: hidden; overflow-y: auto; display: none; padding: 4px 0;
            background: #2b2b2b; color: #f2f2f2; border: 1px solid #454545; border-radius: 6px; box-shadow: 0 8px 24px rgba(0,0,0,.45); }
    .menu.open { display: block; }
    .menu.left { right: auto; left: 0; }
    /* Opens upwards from the panel docked at the bottom of the page. */
    .menu.up { top: auto; bottom: calc(100% + 3px); }
    .row { display: flex; width: 100%; align-items: baseline; gap: 14px; padding: 7px 14px; color: #f2f2f2; font-size: 13px; font-weight: 400; }
    .row:hover { background: #3d3d3d; }
    .row .name { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .row .detail { flex: none; max-width: 55%; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: #e0e0e0; }
    .row.all { font-weight: 600; }
    .sep { height: 1px; background: #454545; margin: 4px 0; }
    .info { padding: 7px 14px; font-size: 12px; color: #bdbdbd; line-height: 1.4; }
    .info.error { color: #ff9b8a; }
    .help { padding: 9px 14px; font-size: 12px; color: #e8e8e8; background: #353535; line-height: 1.45; }
    .spinner { display: inline-block; width: 10px; height: 10px; border: 2px solid #8eb0d8; border-top-color: transparent; border-radius: 50%;
               animation: spin .8s linear infinite; vertical-align: -1px; margin-right: 8px; }
    @keyframes spin { to { transform: rotate(360deg); } }
  `;

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  function buildPanel() {
    host = document.createElement('edm-download-panel');
    const root = host.attachShadow({ mode: 'closed' });
    const style = el('style'); style.textContent = CSS;
    const wrap = el('div', 'wrap');
    const bar = el('div', 'bar');
    const main = el('button', 'main');
    const label = el('span', 'label');
    main.append(el('span', 'arrow'), label);
    const help = el('button', 'small', '?'); help.title = t('panelHelpTitle');
    const close = el('button', 'small', '✕'); close.title = t('panelHide');
    bar.append(main, help, close);
    const menu = el('div', 'menu');
    wrap.append(bar, menu);
    root.append(style, wrap);
    // Keep the page (players toggle play on click) out of the panel's events.
    for (const type of ['click', 'mousedown', 'mouseup', 'pointerdown', 'pointerup', 'dblclick', 'contextmenu', 'wheel']) {
      host.addEventListener(type, (e) => e.stopPropagation());
    }
    main.addEventListener('click', () => (menuOpen ? closeMenu() : openMenu()));
    help.addEventListener('click', () => { openMenu(true); });
    close.addEventListener('click', () => {
      if (target && target.docked) dockDismissed = true;
      else if (target) dismissed.add(target.el);
      closeMenu();
      hidePanel();
    });
    ui = { wrap, main, label, menu };
    document.documentElement.appendChild(host);
  }

  function hidePanel() {
    if (ui) ui.wrap.style.display = 'none';
    target = null;
    closeMenu();
  }

  /**
   * The box the user sees: players often position the <video> oddly (YouTube moves it off-screen until playback),
   * so the enclosing player container wins when there is one.
   */
  function playerRect(media, isAudio) {
    const own = media.getBoundingClientRect();
    if (isAudio) return own;
    const box = media.closest('.html5-video-player, .video-js, .jwplayer, .plyr, [class*="player" i], [id*="player" i]');
    if (box && box !== document.body) {
      const r = box.getBoundingClientRect();
      if (r.width >= 200 && r.height >= 110 && r.width <= innerWidth * 1.05 && r.height <= innerHeight * 1.5) return r;
    }
    return own;
  }

  function visibleRect(media) {
    const isAudio = looksAudio(media);
    const r = playerRect(media, isAudio);
    const [minW, minH] = isAudio ? [200, 28] : [200, 110];
    if (r.width < minW || r.height < minH) return null;
    if (r.bottom < 0 || r.right < 0 || r.top > innerHeight || r.left > innerWidth) return null;
    const cs = getComputedStyle(media);
    if (cs.visibility === 'hidden' || cs.display === 'none' || Number(cs.opacity) === 0) return null;
    return r;
  }

  // YouTube plays previews when the pointer rests on a thumbnail (home page, search, channels): those are not the
  // video the user is watching, and the page address is not a video either.
  const YT_VIDEO_PAGE = /^\/(watch$|shorts\/|live\/|embed\/)/;
  const YT_PREVIEW = '#inline-preview-player, ytd-video-preview, ytd-thumbnail, #video-preview, ytd-rich-item-renderer';
  const skipOnYouTube = (media) => isYouTube && (!YT_VIDEO_PAGE.test(location.pathname) || !!media.closest(YT_PREVIEW));

  // Audio the page plays without a visible player (a hidden <audio>, custom controls, or a file the network layer
  // caught): like IDM, a "Download audio from this page" panel docked at the bottom of the page (top frame only).
  let audioOnPage = false;
  let dockDismissed = false;
  const isTop = window === window.top;

  // Players are often built from web components (archive.org, many podcast players): their media elements sit in
  // shadow roots that document.querySelectorAll does not reach. Those are looked up every 2 seconds.
  function mediaElements() {
    if (Date.now() - shadowScanAt > 2000) {
      shadowScanAt = Date.now();
      const found = [];
      const walk = (root) => {
        for (const e of root.querySelectorAll('*')) {
          if (e.shadowRoot) { found.push(...e.shadowRoot.querySelectorAll('video, audio')); walk(e.shadowRoot); }
        }
      };
      try { walk(document); } catch { /* keep the last result */ }
      shadowMedia = found;
    }
    return [...document.querySelectorAll('video, audio'), ...shadowMedia.filter((m) => m.isConnected)];
  }

  // Sound played through a <video> element (archive.org does this) counts as audio.
  const AUDIO_SRC = /\.(mp3|m4a|aac|ogg|oga|opus|wav|flac|weba|wma)(\?|#|$)/i;
  const sourceOf = (m) => m.currentSrc || m.src || (m.querySelector('source') || {}).src || '';
  const looksAudio = (m) => m.tagName === 'AUDIO' || AUDIO_SRC.test(sourceOf(m)) || (m.readyState >= 1 && !m.videoWidth && !m.videoHeight);

  /** The player under the pointer, else the largest visible one. */
  function pickTarget() {
    let best = null;
    for (const media of mediaElements()) {
      if (dismissed.has(media) || skipOnYouTube(media)) continue;
      const isAudio = looksAudio(media);
      // A video player may not have loaded its stream yet (no autoplay); audio without a source is a placeholder.
      if (isAudio && !sourceOf(media)) continue;
      // A recording (30 s or longer, one being played, or one with visible controls), not a notification sound
      // or endless live radio.
      if (isAudio && media.duration !== Infinity) {
        const d = media.duration, box = media.getBoundingClientRect();
        if (d >= 30 || (!media.paused && !(d < 30)) || (Number.isNaN(d) && box.width >= 150 && box.height >= 20)) audioOnPage = true;
      }
      const r = visibleRect(media);
      if (!r) continue;
      const hovered = pointer.x >= r.left && pointer.x <= r.right && pointer.y >= r.top && pointer.y <= r.bottom;
      const score = (hovered ? 1e9 : 0) + r.width * r.height;
      if (!best || score > best.score) best = { el: media, rect: r, isAudio, score };
    }
    return best;
  }

  function update() {
    if (!enabled || document.fullscreenElement) { if (ui && !menuOpen) hidePanel(); return; }
    if (menuOpen && target && target.el && document.contains(target.el)) {
      const r = visibleRect(target.el);
      if (r) { place(r); return; }
    }
    const next = pickTarget();
    if (!next) {
      if (isTop && audioOnPage && !dockDismissed && !isYouTube) { showDocked(); return; }
      if (ui) hidePanel();
      return;
    }
    if (!ui) buildPanel();
    if (!target || target.el !== next.el) {
      closeMenu();
      target = { el: next.el, isAudio: next.isAudio, label: next.isAudio ? t('panelAudio') : t('panelVideo') };
      ui.label.textContent = target.label;
    }
    ui.wrap.style.display = 'block';
    place(next.rect);
    prefetch();
  }

  function showDocked() {
    if (!ui) buildPanel();
    if (!target || !target.docked) {
      closeMenu();
      target = { el: null, isAudio: true, docked: true, label: t('panelPageAudio') };
      ui.label.textContent = target.label;
    }
    ui.wrap.style.display = 'block';
    const h = ui.wrap.firstChild.offsetHeight || 26;
    ui.wrap.style.left = '8px';
    ui.wrap.style.top = Math.round(innerHeight - h - 8) + 'px';
    prefetch();
  }

  api.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === 'audioCaptured' && isTop) { audioOnPage = true; update(); }
  });

  // Like IDM: the qualities are read while the user watches, so the menu opens with them. Once per address
  // (YouTube changes the address without reloading the page).
  let prefetchedUrl = '';
  function prefetch() {
    const url = location.href;
    if (url === prefetchedUrl || !/^https?:/i.test(url)) return;
    prefetchedUrl = url;
    send({ type: 'prefetch', url, referer: url });
  }

  /** Above the player's top-right corner (inside it when there is no room above), like IDM. */
  function place(r) {
    const w = ui.wrap.offsetWidth || 190, h = ui.wrap.firstChild.offsetHeight || 26;
    let top = r.top - h - 3;
    if (top < 2) top = r.top + 4;
    const left = Math.max(2, Math.min(r.right - w - 4, innerWidth - w - 4));
    ui.wrap.style.top = Math.round(top) + 'px';
    ui.wrap.style.left = Math.round(left) + 'px';
  }

  function closeMenu() {
    menuOpen = false;
    menuToken++;
    if (ui) { ui.menu.classList.remove('open'); ui.menu.textContent = ''; }
  }

  // ---------- Menu entries ----------
  const isYouTube = /(^|\.)(youtube\.com|youtu\.be|youtube-nocookie\.com)$/i.test(location.hostname);

  function fmtSize(n) {
    if (!n) return '';
    if (n < 1000) return t('sizeBytes', [String(n)]);
    const u = ['KB', 'MB', 'GB', 'TB'];
    let v = n / 1000, i = 0;
    while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
    return v.toFixed(i === 0 ? 0 : 1) + ' ' + u[i];
  }
  function fmtDuration(sec) {
    sec = Math.round(sec || 0);
    if (!sec) return '';
    const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60;
    if (h) return t('durHours', [String(h), String(m)]);
    if (m) return t('durMinutes', [String(m), String(s)]);
    return t('durSeconds', [String(s)]);
  }
  const fileKind = (ext) => t('fileKind', [String(ext || '').toUpperCase()]);
  const join = (...parts) => parts.filter(Boolean).join(', ');
  const nameOf = (url) => { try { return decodeURIComponent(new URL(url).pathname.split('/').filter(Boolean).pop() || '') || url; } catch { return url; } };
  function extOf(url, type) {
    const m = /\.([a-z0-9]{2,5})$/i.exec(nameOf(url).split('?')[0]);
    if (m) return m[1].toLowerCase();
    return { 'video/mp4': 'mp4', 'video/webm': 'webm', 'audio/mpeg': 'mp3', 'audio/mp4': 'm4a', 'video/mp2t': 'ts', 'text/vtt': 'vtt', 'application/x-subrip': 'srt' }[(type || '').toLowerCase()] || '';
  }
  /** "Show title - Site" → a usable file name. */
  const cleanTitle = (s) => String(s || '').replace(/[\\/:*?"<>|]+/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 150);

  /**
   * IDM-like entries for what the tab captured: one per HLS resolution, subtitles, video/audio files.
   * Each entry: { name, detail, download, group } — `group` lets "Download all" take one resolution per stream.
   */
  function capturedEntries(items, title) {
    const out = [];
    const sorted = [...items].sort((a, b) => (b.time || 0) - (a.time || 0));
    // The variant (resolution) and subtitle playlists of a master playlist are listed under the master, not on
    // their own; the one the player loaded tells the master's duration and encryption.
    const byUrl = new Map(items.filter((m) => m.kind === 'hls').map((m) => [m.url, m]));
    const children = new Set();
    for (const m of byUrl.values()) {
      const d = m.details;
      if (!d || !d.variants) continue;
      for (const v of [...d.variants, ...(d.subtitles || [])]) {
        const child = byUrl.get(v.url);
        if (!child) continue;
        children.add(v.url);
        const cd = child.details;
        if (cd && !d.duration && cd.duration) Object.assign(d, { duration: cd.duration, live: cd.live, encrypted: cd.encrypted || d.encrypted });
      }
    }
    for (const m of sorted) {
      if (m.kind === 'hls') {
        if (children.has(m.url)) continue;
        const d = m.details;
        if (d && d.encrypted && d.encrypted !== 'AES-128') {
          out.push({ name: title, detail: join(fileKind('mp4'), t('protectedStream')), download: null, group: m.key });
          continue;
        }
        const length = d ? (d.live && !d.duration ? t('liveStream') : fmtDuration(d.duration)) : '';
        const manifest = m.manifest || undefined;
        if (d && d.variants && d.variants.length) {
          d.variants.forEach((v, i) => {
            const res = v.height ? `${v.width}x${v.height}` : '';
            const size = v.bandwidth && d.duration ? '~' + fmtSize(Math.round((v.bandwidth * d.duration) / 8)) : '';
            const format = v.height ? `b[height=${v.height}]/bv*[height=${v.height}]+ba/bv*[height<=${v.height}]+ba/b` : null;
            out.push({ name: title, detail: join(fileKind('mp4'), res, length, size), group: m.key, rank: i,
              download: { kind: 'video', url: m.url, format, title, manifest } });
          });
        } else {
          out.push({ name: title, detail: join(fileKind('mp4'), d ? length : t('hlsStream')), group: m.key, rank: 0,
            download: { kind: 'video', url: m.url, title, manifest } });
        }
      } else if (m.kind === 'dash') {
        out.push({ name: title, detail: join(fileKind('mp4'), 'DASH'), group: m.key, rank: 0,
          download: { kind: 'video', url: m.url, title, manifest: m.manifest || undefined } });
      } else {
        const ext = extOf(m.url, m.type) || (m.kind === 'subtitle' ? 'srt' : 'mp4');
        const label = m.kind === 'subtitle' && m.name ? `${title} (${m.name})` : title;
        out.push({ name: label, detail: join(fileKind(ext), fmtSize(m.size)), group: m.key, rank: 0,
          download: { kind: 'file', url: m.url, filename: `${cleanTitle(label)}.${ext}` } });
      }
    }
    return out;
  }

  // ---------- Menu rendering ----------
  function addRow(cls, name, detail, onClick) {
    const b = el('button', 'row ' + cls);
    b.append(el('span', 'name', name), el('span', 'detail', detail || ''));
    b.title = name + (detail ? '  —  ' + detail : '');
    if (onClick) b.addEventListener('click', onClick); else b.style.opacity = '0.55';
    ui.menu.append(b);
    return b;
  }
  const addInfo = (text, cls = '') => { const d = el('div', 'info ' + cls, text); ui.menu.append(d); return d; };
  const addSep = () => ui.menu.append(el('div', 'sep'));

  async function download(opts) {
    const r = await send({ type: 'download', referer: location.href, quiet: true, ...opts });
    return !!(r && r.ok);
  }

  function flash(ok) {
    if (!ui) return;
    const old = target ? target.label : t('panelVideo');
    ui.label.textContent = ok ? t('panelSent') : t('panelOffline');
    ui.wrap.classList.add('flash');
    setTimeout(() => { if (ui) { ui.label.textContent = old; ui.wrap.classList.remove('flash'); } }, 2500);
  }

  /** Renders the entries, numbered; "Download all" takes the best resolution of each stream plus every file. */
  function renderEntries(entries, startAt) {
    let n = startAt;
    for (const e of entries) {
      n++;
      addRow('', `${n}. ${e.name}`, e.detail, e.download ? async () => { closeMenu(); flash(await download(e.download)); } : null);
    }
    return n;
  }

  async function openMenu(withHelp = false) {
    if (!target) return;
    const token = ++menuToken;
    menuOpen = true;
    const pageUrl = location.href;
    const m = ui.menu;
    m.textContent = '';
    m.classList.add('open');
    // Open towards the side with room.
    const at = ui.wrap.getBoundingClientRect();
    m.classList.toggle('left', at.right < Math.min(560, innerWidth - 16));
    m.classList.toggle('up', at.top > innerHeight / 2);
    m.style.maxHeight = Math.max(160, Math.min(380, (at.top > innerHeight / 2 ? at.top : innerHeight - at.bottom) - 12)) + 'px';
    if (withHelp) { m.append(el('div', 'help', t('panelHelp'))); addSep(); }

    const loading = addInfo('');
    loading.prepend(el('span', 'spinner'));
    loading.append(t('panelLoading'));
    const formatsPromise = send({ type: 'formats', url: pageUrl, referer: pageUrl });

    // 1) What this tab captured (instant, plus HLS details read from the playlists).
    const media = (await send({ type: 'tabMedia' })) || { items: [] };
    if (token !== menuToken) return;
    const title = cleanTitle(media.title || document.title) || nameOf(pageUrl);
    const direct = target.el ? target.el.currentSrc || target.el.src || '' : '';
    const items = (media.items || []).slice();
    if (/^https?:/i.test(direct) && !items.some((i) => i.url === direct)) {
      items.push({ url: direct, kind: 'media', type: '', size: 0, key: direct, time: 0 });
    }
    const captured = isYouTube ? [] : capturedEntries(items, title);
    loading.remove();

    let shown = 0;
    const allRow = el('button', 'row all');
    if (captured.filter((e) => e.download).length > 1) {
      allRow.append(el('span', 'name', t('panelDownloadAll')));
      allRow.addEventListener('click', async () => {
        closeMenu();
        const best = new Map();
        for (const e of captured) if (e.download && (!best.has(e.group) || e.rank < best.get(e.group).rank)) best.set(e.group, e);
        let ok = true;
        for (const e of best.values()) ok = (await download(e.download)) && ok;
        flash(ok);
      });
      m.append(allRow);
      addSep();
    }
    if (isYouTube) {
      shown = renderEntries([{ name: title, detail: t('panelDefaultDetail'), download: { kind: 'video', url: pageUrl } }], shown);
      // A video played from a playlist: the app's playlist dialog lists all of its videos.
      if (new URL(pageUrl).searchParams.get('list')) {
        addRow('', t('panelPlaylist'), t('panelPlaylistDetail'), async () => {
          closeMenu();
          const r = await send({ type: 'openPlaylist', url: pageUrl });
          flash(!!(r && r.ok));
        });
      }
    }
    shown = renderEntries(captured, shown);

    // 2) Qualities yt-dlp reads from the page — on YouTube always, elsewhere when yt-dlp knows the site
    //    (its "generic" result would only repeat the streams above).
    const pending = addInfo('');
    pending.prepend(el('span', 'spinner'));
    pending.append(t('panelLoading'));
    const formats = await formatsPromise;
    if (token !== menuToken) return;
    pending.remove();
    const known = formats && formats.ok && formats.formats && formats.formats.length && (isYouTube || !/^generic$/i.test(formats.extractor || ''));
    if (known) {
      const name = cleanTitle(formats.title) || title;
      const entries = formats.formats
        .filter((f) => !target.isAudio || f.isAudio)
        .map((f) => ({ name, detail: join(f.label, f.detail), download: { kind: 'video', url: pageUrl, format: f.format, audio: f.audio } }));
      if (shown) addSep();
      shown = renderEntries(entries, shown);
    } else if (formats && formats.offline) {
      addInfo(t('panelOffline'), 'error');
    } else if (isYouTube && formats && !formats.ok) {
      addInfo(t('panelFormatsFailed', [String(formats.error || '').slice(0, 200)]), 'error');
    }
    if (!shown) addInfo(t('panelNothing'));
  }

  document.addEventListener('mousedown', (e) => { if (menuOpen && host && !e.composedPath().includes(host)) closeMenu(); }, true);
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && menuOpen) closeMenu(); }, true);
  document.addEventListener('mousemove', (e) => { pointer = { x: e.clientX, y: e.clientY }; }, { passive: true, capture: true });
  let raf = 0;
  const schedule = () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; update(); }); };
  addEventListener('scroll', schedule, { passive: true, capture: true });
  addEventListener('resize', schedule, { passive: true });
  document.addEventListener('play', schedule, true);
  setInterval(update, 500);
})();
