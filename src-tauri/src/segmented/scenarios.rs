//! Scenario tests: the downloader against a local server that behaves like the awkward ones met in the wild
//! (a limit on connections, no Range support, throttling, dropped connections, forms, bot checks…). Each test names
//! the site or the report that showed the behaviour; a new one found in use belongs here before it is fixed.

use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicUsize;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const MB: usize = 1024 * 1024;

#[derive(Clone, Default)]
struct Behavior {
    size: usize,
    /// Answers Range requests with 206; otherwise every request gets the whole file with 200.
    ranges: bool,
    /// Says "Accept-Ranges: bytes" but answers every request with the whole file anyway.
    claims_ranges: bool,
    /// At most this many downloads at once; the ones beyond get 403 (files-cdn.com: 4).
    max_connections: Option<usize>,
    /// The first n segment requests (not the probe) get 429.
    throttle_first: usize,
    /// The first answer for each start offset breaks off after this many bytes.
    drop_after: Option<usize>,
    /// The first ranged answer past offset 0 starts at 0 whatever was asked.
    wrong_range_once: bool,
    /// Every request gets this status (403: an expired link, 404…).
    status: Option<u16>,
    /// Cloudflare's challenge: 403 + cf-mitigated.
    bot_check: bool,
    /// "/start" redirects to "/file".
    redirect: bool,
    /// The file answers a POST with exactly this body; anything else gets an HTML page (uupdump's get.php).
    post_only: Option<&'static str>,
    /// Headers the server checks; 403 without them.
    require: Vec<(&'static str, &'static str)>,
    /// No Content-Length: the body ends when the connection closes.
    no_length: bool,
    disposition: Option<&'static str>,
}

#[derive(Default)]
struct Stats {
    requests: AtomicUsize,
    open: AtomicUsize,
    peak_open: AtomicUsize,
    refused: AtomicUsize,
    throttled: AtomicUsize,
    dropped: Mutex<HashSet<usize>>,
    wrong_sent: AtomicBool,
    methods: Mutex<Vec<String>>,
}

/// Bytes that differ along the file, so a piece written at the wrong offset shows.
fn content(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8 ^ (i >> 10) as u8).collect()
}

async fn serve(b: Behavior) -> (String, Arc<Stats>, Arc<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let data = Arc::new(content(b.size));
    let (b, stats) = (Arc::new(b), Arc::new(Stats::default()));
    let (b2, s2, d2) = (b.clone(), stats.clone(), data.clone());
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            tokio::spawn(handle(s, b2.clone(), s2.clone(), d2.clone()));
        }
    });
    (base, stats, data)
}

async fn read_request(s: &mut TcpStream) -> Option<(String, String, HashMap<String, String>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: HashMap<String, String> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())).collect();
    let length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some((method, path, headers, body))
}

async fn reply(s: &mut TcpStream, status: u16, headers: &[(&str, String)], body: &[u8]) {
    let mut head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = s.write_all(head.as_bytes()).await;
    let _ = s.write_all(body).await;
    let _ = s.shutdown().await;
}

struct Open(Arc<Stats>);
impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn handle(mut s: TcpStream, b: Arc<Behavior>, st: Arc<Stats>, data: Arc<Vec<u8>>) {
    let Some((method, path, headers, body)) = read_request(&mut s).await else { return };
    st.requests.fetch_add(1, Ordering::SeqCst);
    st.methods.lock().unwrap().push(method.clone());
    let header = |name: &str| headers.get(name).cloned().unwrap_or_default();
    let page = b"<!DOCTYPE html><html><body>a page, not the file</body></html>".to_vec();
    let html = [("Content-Type", "text/html; charset=UTF-8".to_string())];

    if let Some(code) = b.status {
        return reply(&mut s, code, &html, &page).await;
    }
    if b.redirect && path == "/start" {
        return reply(&mut s, 302, &[("Location", "/file".into())], b"").await;
    }
    if b.bot_check {
        let h = [("Content-Type", "text/html".to_string()), ("cf-mitigated", "challenge".into()), ("Server", "cloudflare".into())];
        return reply(&mut s, 403, &h, &page).await;
    }
    if b.require.iter().any(|(k, v)| header(k) != *v) {
        return reply(&mut s, 403, &html, &page).await;
    }
    if let Some(expected) = b.post_only {
        if method != "POST" || body != expected.as_bytes() {
            return reply(&mut s, 200, &html, &page).await;
        }
    }
    let range = header("range");
    let is_probe = range == "bytes=0-0";
    if !is_probe && st.throttled.load(Ordering::SeqCst) < b.throttle_first {
        st.throttled.fetch_add(1, Ordering::SeqCst);
        return reply(&mut s, 429, &html, &page).await;
    }
    let open = st.open.fetch_add(1, Ordering::SeqCst) + 1;
    let _open = Open(st.clone());
    if b.max_connections.is_some_and(|m| open > m) {
        st.refused.fetch_add(1, Ordering::SeqCst);
        return reply(&mut s, 403, &html, &page).await;
    }
    st.peak_open.fetch_max(open, Ordering::SeqCst);

    let size = data.len();
    let asked = range.strip_prefix("bytes=").and_then(|r| r.split_once('-')).and_then(|(a, z)| {
        let a: usize = a.parse().ok()?;
        let z: usize = if z.is_empty() { size - 1 } else { z.parse::<usize>().ok()?.min(size - 1) };
        (a <= z).then_some((a, z))
    });
    let ranged = b.ranges && b.post_only.is_none();
    let (status, mut start, end) = match asked {
        Some((a, z)) if ranged => (206, a, z),
        _ => (200, 0, size - 1),
    };
    if status == 206 && start > 0 && b.wrong_range_once && !st.wrong_sent.swap(true, Ordering::SeqCst) {
        start = 0;
    }
    let mut head = format!("HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Type: application/octet-stream\r\n");
    if !b.no_length {
        head.push_str(&format!("Content-Length: {}\r\n", end - start + 1));
    }
    if status == 206 {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
    }
    if ranged || b.claims_ranges {
        head.push_str("Accept-Ranges: bytes\r\n");
    }
    if let Some(d) = b.disposition {
        head.push_str(&format!("Content-Disposition: {d}\r\n"));
    }
    head.push_str("\r\n");
    if s.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    if method == "HEAD" {
        return;
    }
    let mut stop = end + 1;
    if let Some(n) = b.drop_after.filter(|_| !is_probe) {
        if st.dropped.lock().unwrap().insert(start) {
            stop = (start + n).min(stop);
        }
    }
    // Small pieces with pauses, so connections overlap as they do over a real network.
    let mut at = start;
    while at < stop {
        let to = (at + 16 * 1024).min(stop);
        if s.write_all(&data[at..to]).await.is_err() {
            return;
        }
        at = to;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let _ = s.shutdown().await;
}

struct Outcome {
    result: Result<(), DlError>,
    file: Vec<u8>,
    probe: Option<ProbeResult>,
    segments: Vec<SegmentInfo>,
    took: Duration,
}

async fn fetch(url: &str, setup: impl FnOnce(&mut Config)) -> Outcome {
    fetch_with(url, setup, None, |_| false).await
}

/// Downloads `url` into a scratch folder; `stop_when` (received bytes) cancels it, as a pause does.
async fn fetch_with(url: &str, setup: impl FnOnce(&mut Config), keep: Option<PathBuf>, stop_when: impl Fn(i64) -> bool + Send + Sync + 'static) -> Outcome {
    let scratch = keep.is_none();
    let part = keep.unwrap_or_else(|| std::env::temp_dir().join(format!("edm-scenario-{}", Uuid::new_v4())).join("file.part"));
    let mut cfg = Config::new(Url::parse(url).unwrap(), vec![], part.clone(), 8);
    setup(&mut cfg);
    let probe = Arc::new(Mutex::new(None));
    let segments = Arc::new(Mutex::new(vec![]));
    let cancel = CancellationToken::new();
    let (p1, s1, c1) = (probe.clone(), segments.clone(), cancel.clone());
    let started = Instant::now();
    let result = timeout(
        Duration::from_secs(90),
        download(cfg, cancel, move |ev| match ev {
            Event::Probe(p) => *p1.lock().unwrap() = Some(p),
            Event::Progress(received, segs) => {
                *s1.lock().unwrap() = segs;
                if stop_when(received) {
                    c1.cancel();
                }
            }
        }),
    )
    .await
    .expect("the download hangs");
    let took = started.elapsed();
    let file = std::fs::read(&part).unwrap_or_default();
    if scratch {
        let _ = std::fs::remove_dir_all(part.parent().unwrap());
    }
    let segments = segments.lock().unwrap().clone();
    let probe = probe.lock().unwrap().clone();
    Outcome { result, file, probe, segments, took }
}

fn assert_same(o: &Outcome, data: &[u8]) {
    if let Err(e) = &o.result {
        panic!("download failed: {e}");
    }
    assert_eq!(o.file.len(), data.len(), "file size");
    if let Some(at) = o.file.iter().zip(data).position(|(a, b)| a != b) {
        panic!("file differs from byte {at}");
    }
}

fn failed_with(o: &Outcome) -> String {
    match &o.result {
        Err(DlError::Failed(m)) => m.clone(),
        other => panic!("expected a failure, got {:?}", other.as_ref().map(|_| ())),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_ranges() {
    let (base, st, data) = serve(Behavior { size: 4 * MB, ranges: true, ..Default::default() }).await;
    let o = fetch(&format!("{base}/file"), |_| {}).await;
    assert_same(&o, &data);
    assert!(st.peak_open.load(Ordering::SeqCst) > 1, "connections did not run in parallel");
}

/// files-cdn.com: 403 for the 5th connection while 4 download (reported 2026-10-23; IDM downloads it).
#[tokio::test(flavor = "multi_thread")]
async fn connection_limit_of_four() {
    let (base, st, data) = serve(Behavior { size: 6 * MB, ranges: true, max_connections: Some(4), ..Default::default() }).await;
    let o = fetch(&format!("{base}/file"), |_| {}).await;
    assert_same(&o, &data);
    assert!(st.refused.load(Ordering::SeqCst) > 0, "the limit was never hit");
    assert!(st.peak_open.load(Ordering::SeqCst) <= 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_limit_of_one() {
    let (base, _, data) = serve(Behavior { size: 3 * MB, ranges: true, max_connections: Some(1), ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_range_support() {
    let (base, _, data) = serve(Behavior { size: 2 * MB, ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

/// "Accept-Ranges: bytes" in the probe, the whole file for every segment: one stream after all.
#[tokio::test(flavor = "multi_thread")]
async fn ranges_claimed_but_ignored() {
    let (base, _, data) = serve(Behavior { size: 3 * MB, claims_ranges: true, ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn throttled_with_429() {
    let (base, _, data) = serve(Behavior { size: 3 * MB, ranges: true, throttle_first: 3, ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn segments_break_off_and_continue() {
    let (base, _, data) = serve(Behavior { size: 4 * MB, ranges: true, drop_after: Some(100_000), ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

/// Without ranges a broken stream can only start over.
#[tokio::test(flavor = "multi_thread")]
async fn single_stream_breaks_off_and_starts_over() {
    let (base, _, data) = serve(Behavior { size: 2 * MB, drop_after: Some(300_000), ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn range_answer_for_the_wrong_offset() {
    let (base, _, data) = serve(Behavior { size: 4 * MB, ranges: true, wrong_range_once: true, ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_size() {
    let (base, _, data) = serve(Behavior { size: MB + 123, no_length: true, ..Default::default() }).await;
    assert_same(&fetch(&format!("{base}/file"), |_| {}).await, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_to_the_file() {
    let (base, _, data) = serve(Behavior { size: 3 * MB, ranges: true, redirect: true, ..Default::default() }).await;
    let o = fetch(&format!("{base}/start"), |_| {}).await;
    assert_same(&o, &data);
    assert!(o.probe.unwrap().final_url.unwrap().ends_with("/file"));
}

/// uupdump's get.php: the zip answers the form (POST); a GET gets the page (reported 2026-10-22).
#[tokio::test(flavor = "multi_thread")]
async fn answer_to_a_form() {
    let disposition = "attachment; filename=\"26300_convert.zip\"";
    let b = Behavior { size: 8213, ranges: true, post_only: Some("autodl=2&updates=1"), disposition: Some(disposition), ..Default::default() };
    let (base, st, data) = serve(b).await;
    let o = fetch(&format!("{base}/get.php?id=1"), |c| {
        c.post_body = Some(b"autodl=2&updates=1".to_vec());
        c.headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
    })
    .await;
    assert_same(&o, &data);
    assert_eq!(o.probe.unwrap().suggested_file_name.as_deref(), Some("26300_convert.zip"));
    assert_eq!(*st.methods.lock().unwrap(), vec!["POST"], "the form is sent once");
}

#[tokio::test(flavor = "multi_thread")]
async fn headers_the_site_checks() {
    let b = Behavior { size: 2 * MB, ranges: true, require: vec![("referer", "https://site.example/page"), ("cookie", "s=1")], ..Default::default() };
    let (base, _, data) = serve(b).await;
    let o = fetch(&format!("{base}/file"), |c| {
        c.headers.push(("Referer".into(), "https://site.example/page".into()));
        c.headers.push(("Cookie".into(), "s=1".into()));
    })
    .await;
    assert_same(&o, &data);
}

#[tokio::test(flavor = "multi_thread")]
async fn bot_check_goes_back_to_the_browser() {
    let (base, _, _) = serve(Behavior { size: MB, ranges: true, bot_check: true, ..Default::default() }).await;
    let o = fetch(&format!("{base}/file"), |_| {}).await;
    assert!(matches!(o.result, Err(DlError::BrowserOnly)));
}

/// An expired link: refused at once, not after a minute of retries.
#[tokio::test(flavor = "multi_thread")]
async fn refused_address_fails_fast() {
    let (base, _, _) = serve(Behavior { size: MB, ranges: true, status: Some(403), ..Default::default() }).await;
    let o = fetch(&format!("{base}/file"), |_| {}).await;
    assert_eq!(failed_with(&o), http_status_message(403));
    assert!(o.took < Duration::from_secs(5), "took {:?}", o.took);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_file() {
    let (base, _, _) = serve(Behavior { size: MB, ranges: true, status: Some(404), ..Default::default() }).await;
    assert_eq!(failed_with(&fetch(&format!("{base}/file"), |_| {}).await), http_status_message(404));
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_resume() {
    let (base, _, data) = serve(Behavior { size: 6 * MB, ranges: true, ..Default::default() }).await;
    let url = format!("{base}/file");
    let part = std::env::temp_dir().join(format!("edm-scenario-{}", Uuid::new_v4())).join("file.part");
    let half = (data.len() / 2) as i64;
    let first = fetch_with(&url, |_| {}, Some(part.clone()), move |r| r > half).await;
    assert!(matches!(first.result, Err(DlError::Cancelled)));
    let done: i64 = first.segments.iter().map(|s| s.received).sum();
    assert!(done > 0 && done < data.len() as i64, "paused with {done} bytes");
    let segments = first.segments.clone();
    let total = data.len() as i64;
    let second = fetch_with(&url, move |c| {
        c.resume_segments = segments;
        c.resume_total = total;
    }, Some(part.clone()), |_| false)
    .await;
    assert_same(&second, &data);
    let _ = std::fs::remove_dir_all(part.parent().unwrap());
}
