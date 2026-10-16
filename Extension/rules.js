// EDM capture rules, applied in the extension itself (background and content scripts). They mirror
// Settings::should_capture in the app, whose /rules endpoint provides them, so a download can be taken over
// before the browser requests a single byte of it.
// eslint-disable-next-line no-var
var EDM_RULES = (() => {
  'use strict';

  /** `*` and `?` wildcards, case-insensitive (util::wildcard_match). */
  function wildcard(pattern, text) {
    const p = String(pattern).toLowerCase(), s = String(text).toLowerCase();
    let pi = 0, si = 0, star = -1, mark = 0;
    while (si < s.length) {
      if (pi < p.length && (p[pi] === '?' || p[pi] === s[si])) { pi++; si++; }
      else if (pi < p.length && p[pi] === '*') { star = pi++; mark = si; }
      else if (star >= 0) { pi = star + 1; si = ++mark; }
      else return false;
    }
    while (pi < p.length && p[pi] === '*') pi++;
    return pi === p.length;
  }

  /** "Setup.EXE" → "exe"; no extension for ".bashrc" or "name". */
  function extension(name) {
    name = String(name || '').split(/[\\/]/).pop();
    const i = name.lastIndexOf('.');
    return i > 0 && i < name.length - 1 ? name.slice(i + 1).toLowerCase() : '';
  }

  /** Extension of the URL's last path segment. */
  function urlExtension(url) {
    try {
      const last = new URL(url).pathname.split('/').pop() || '';
      let name = last;
      try { name = decodeURIComponent(last); } catch { /* keep it encoded */ }
      return extension(name);
    } catch { return ''; }
  }

  /** Should the app take this URL (file name from the link or Content-Disposition, if known)? */
  function capture(rules, url, filename) {
    if (!rules) return false;
    let u;
    try { u = new URL(url); } catch { return false; }
    const host = u.hostname.toLowerCase();
    if ((rules.excludedSites || []).some((p) => wildcard(p, host))) return false;
    const full = u.href.toLowerCase();
    const excluded = (rules.excludedAddresses || []).some((p) => (/[*?]/.test(p) ? wildcard(p, full) : full.startsWith(p)));
    if (excluded) return false;
    const ext = extension(filename) || urlExtension(url);
    return !!ext && (rules.captureTypes || []).some((p) => wildcard(p, ext));
  }

  /** File name from a Content-Disposition header (filename* first). */
  function dispositionName(cd) {
    cd = String(cd || '');
    let m = /filename\*\s*=\s*([^']*)'[^']*'([^;]+)/i.exec(cd);
    if (m) { try { return decodeURIComponent(m[2].trim().replace(/^"|"$/g, '')); } catch { /* fall through */ } }
    m = /filename\s*=\s*("([^"]*)"|[^;]+)/i.exec(cd);
    return m ? (m[2] !== undefined ? m[2] : m[1]).trim() : '';
  }

  // Types the browser shows itself (viewer, player, page) instead of downloading: a click on such a link stays
  // with the browser (the video panel offers those); a response of such a type is only taken when it is an attachment.
  const SHOWN_EXT = /^(pdf|mp4|m4v|webm|ogv|ogg|oga|opus|mp3|m4a|aac|wav|flac|txt|htm|html|xhtml|xml|json|svg|png|jpe?g|gif|webp|avif|bmp|ico)$/;
  const SHOWN_TYPE = /^(text\/|image\/|video\/|audio\/|application\/(pdf|json|xml|xhtml\+xml|javascript|ecmascript)|multipart\/x-mixed-replace)/;
  const shownExtension = (ext) => SHOWN_EXT.test(ext || '');
  const shownType = (ct) => SHOWN_TYPE.test(String(ct || '').toLowerCase().trim());

  return { wildcard, extension, urlExtension, capture, dispositionName, shownExtension, shownType };
})();
