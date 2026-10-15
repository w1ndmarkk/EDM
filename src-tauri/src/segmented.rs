//! Downloads one file with N parallel HTTP `Range` requests, falling back to a single stream when the
//! server does not support ranges. Connection count adapts to 429/503 throttling.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, RequestBuilder, Url};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::i18n::t;
use crate::models::SegmentInfo;
use crate::tf;

const MAX_ATTEMPTS: u32 = 12;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const REPORT_INTERVAL: Duration = Duration::from_millis(200);

/// Shared HTTP client. HTTP/1.1 only, so every Range request gets its own TCP connection
/// (HTTP/2 would multiplex all segments onto one connection and defeat the acceleration).
/// No cookie store; reqwest drops Cookie/Authorization headers on cross-host redirects.
pub static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .http1_only()
        .connect_timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(32)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("HTTP client")
});

#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub total_bytes: i64,
    pub supports_ranges: bool,
    pub suggested_file_name: Option<String>,
    pub content_type: Option<String>,
    pub final_url: Option<String>,
}

pub enum Event {
    Probe(ProbeResult),
    /// Bytes received so far and the per-segment state (empty in single-stream mode).
    Progress(i64, Vec<SegmentInfo>),
}

#[derive(Debug)]
pub enum DlError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for DlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DlError::Cancelled => write!(f, "cancelled"),
            DlError::Failed(m) => f.write_str(m),
        }
    }
}

pub struct Config {
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub part_path: PathBuf,
    pub segment_count: usize,
    pub min_segment_size: i64,
    pub resume_segments: Vec<SegmentInfo>,
    pub resume_total: i64,
}

impl Config {
    pub fn new(url: Url, headers: Vec<(String, String)>, part_path: PathBuf, segment_count: usize) -> Self {
        Self { url, headers, part_path, segment_count, min_segment_size: 512 * 1024, resume_segments: vec![], resume_total: -1 }
    }
}

type Sink = Box<dyn Fn(Event) + Send + Sync>;

struct Shared {
    cfg: Config,
    request_url: Mutex<Url>,
    segments: Mutex<Vec<SegmentInfo>>,
    single_mode: AtomicBool,
    single_received: AtomicI64,
    last_report: Mutex<Instant>,
    sink: Sink,
}

impl Shared {
    fn current_received(&self) -> i64 {
        if self.single_mode.load(Ordering::Relaxed) {
            self.single_received.load(Ordering::Relaxed)
        } else {
            self.segments.lock().unwrap().iter().map(|s| s.received).sum()
        }
    }

    fn report(&self, force: bool) {
        {
            let mut last = self.last_report.lock().unwrap();
            if !force && last.elapsed() < REPORT_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        let segments = if self.single_mode.load(Ordering::Relaxed) { vec![] } else { self.segments.lock().unwrap().clone() };
        (self.sink)(Event::Progress(self.current_received(), segments));
    }

    /// Credentials (Cookie/Authorization) are only sent to the host the download was requested for.
    fn request(&self, url: &Url, range: Option<String>) -> RequestBuilder {
        const CREDENTIALS: [&str; 3] = ["cookie", "authorization", "proxy-authorization"];
        let allow_credentials = same_origin(url, &self.cfg.url);
        let mut headers = HeaderMap::new();
        for (k, v) in &self.cfg.headers {
            if !allow_credentials && CREDENTIALS.contains(&k.to_lowercase().as_str()) {
                continue;
            }
            if let (Ok(name), Ok(value)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                headers.insert(name, value);
            }
        }
        headers.insert("accept-encoding", HeaderValue::from_static("identity"));
        headers.insert("cache-control", HeaderValue::from_static("no-cache"));
        if let Some(r) = range.and_then(|r| HeaderValue::from_str(&r).ok()) {
            headers.insert("range", r);
        }
        CLIENT.get(url.clone()).headers(headers)
    }
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme().eq_ignore_ascii_case(b.scheme()) && a.host_str().map(str::to_lowercase) == b.host_str().map(str::to_lowercase)
}

/// Runs the download until it completes, fails or `cancel` fires (pause/cancel). The last segment state
/// is always reported through `sink` before returning, so a paused download can resume where it stopped.
pub async fn download(cfg: Config, cancel: CancellationToken, sink: impl Fn(Event) + Send + Sync + 'static) -> Result<(), DlError> {
    let shared = Arc::new(Shared {
        request_url: Mutex::new(cfg.url.clone()),
        cfg,
        segments: Mutex::new(vec![]),
        single_mode: AtomicBool::new(false),
        single_received: AtomicI64::new(0),
        last_report: Mutex::new(Instant::now() - REPORT_INTERVAL),
        sink: Box::new(sink),
    });
    let result = tokio::select! {
        r = run(shared.clone()) => r,
        _ = cancel.cancelled() => Err(DlError::Cancelled),
    };
    shared.report(true);
    result
}

enum MultiEnd {
    Done,
    Fallback,
}

async fn run(sh: Arc<Shared>) -> Result<(), DlError> {
    let cfg = &sh.cfg;
    let can_resume = cfg.resume_total > 0 && !cfg.resume_segments.is_empty() && cfg.part_path.exists();
    if can_resume {
        *sh.segments.lock().unwrap() = cfg.resume_segments.clone();
        let file = open_file(&cfg.part_path, false, None)?;
        return match run_multi(&sh, file).await? {
            MultiEnd::Done => Ok(()),
            MultiEnd::Fallback => run_single(&sh).await,
        };
    }

    let probe = probe(&sh).await?;
    let (total, ranges) = (probe.total_bytes, probe.supports_ranges);
    (sh.sink)(Event::Probe(probe));

    let count = if ranges { (total / cfg.min_segment_size).clamp(1, cfg.segment_count.max(1) as i64) as usize } else { 1 };
    if ranges && count > 1 {
        *sh.segments.lock().unwrap() = make_segments(total, count);
        let file = open_file(&cfg.part_path, true, Some(total))?;
        match run_multi(&sh, file).await? {
            MultiEnd::Done => Ok(()),
            MultiEnd::Fallback => run_single(&sh).await,
        }
    } else {
        run_single(&sh).await
    }
}

async fn probe(sh: &Shared) -> Result<ProbeResult, DlError> {
    let url = sh.request_url.lock().unwrap().clone();
    let resp = match timeout(REQUEST_TIMEOUT, sh.request(&url, Some("bytes=0-0".into())).send()).await {
        Err(_) => return Err(DlError::Failed(t("The connection timed out"))),
        Ok(Err(e)) => return Err(DlError::Failed(error_text(&e))),
        Ok(Ok(r)) => r,
    };
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(DlError::Failed(http_status_message(status)));
    }
    let h = resp.headers();
    let header = |name: &str| h.get(name).map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());

    let mut total = -1;
    let mut ranges = false;
    if status == 206 {
        // "bytes 0-0/12345"; an unknown total ("/*") means the size cannot be split up front.
        if let Some(t) = header("content-range").and_then(|cr| cr.rsplit('/').next().and_then(|t| t.trim().parse::<i64>().ok())) {
            total = t;
            ranges = true;
        }
    } else {
        total = header("content-length").and_then(|v| v.trim().parse().ok()).unwrap_or(-1);
        let accept = header("accept-ranges").unwrap_or_default().to_lowercase();
        ranges = total > 0 && accept.contains("bytes");
    }
    let final_url = resp.url().clone();
    *sh.request_url.lock().unwrap() = final_url.clone();
    let content_type = header("content-type").and_then(|c| c.split(';').next().map(|s| s.trim().to_lowercase())).filter(|s| !s.is_empty());
    Ok(ProbeResult {
        total_bytes: total,
        supports_ranges: ranges,
        suggested_file_name: header("content-disposition").and_then(|c| file_name_from_content_disposition(&c)),
        content_type,
        final_url: Some(final_url.to_string()),
    })
}

pub fn make_segments(total: i64, count: usize) -> Vec<SegmentInfo> {
    let size = total / count as i64;
    (0..count)
        .map(|i| {
            let start = i as i64 * size;
            let end = if i == count - 1 { total - 1 } else { start + size - 1 };
            SegmentInfo { start, end, received: 0 }
        })
        .collect()
}

fn open_file(path: &Path, create: bool, preallocate: Option<i64>) -> Result<Arc<File>, DlError> {
    let fail = |e: std::io::Error| DlError::Failed(format!("{} ({e})", tf!("Could not create file: {0}", path.display())));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let file = OpenOptions::new().write(true).create(true).truncate(create).open(path).map_err(fail)?;
    if let Some(n) = preallocate.filter(|n| *n > 0) {
        set_sparse(&file);
        file.set_len(n as u64).map_err(fail)?;
    }
    Ok(Arc::new(file))
}

/// NTFS zero-fills everything before a write offset that lies past the valid data length, which stalls the
/// first write of a far segment for seconds on large files. A sparse file has no such cost.
fn set_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let mut returned = 0u32;
    // Best effort: FAT32/exFAT volumes do not support sparse files.
    unsafe {
        DeviceIoControl(file.as_raw_handle() as _, FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut());
    }
}

fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

// MARK: - Multi-connection mode

enum SegOutcome {
    Done,
    /// Connection error or early close; `progressed` resets the attempt counter.
    Retry { error: String, progressed: bool },
    /// 429/503 and friends: lower the connection count and try again later.
    Throttle(u16),
    /// The server ignored the Range header (200): switch to a single stream.
    Fallback,
    Fatal(String),
}

async fn run_multi(sh: &Arc<Shared>, file: Arc<File>) -> Result<MultiEnd, DlError> {
    let n = sh.segments.lock().unwrap().len();
    let seg_count = sh.cfg.segment_count.max(1);
    let mut pending: VecDeque<usize> = {
        let segs = sh.segments.lock().unwrap();
        (0..n).filter(|&i| !segs[i].is_done()).collect()
    };
    let mut limit = seg_count;
    let mut retries = vec![0u32; n];
    let mut throttled_until = Instant::now();
    let mut set: JoinSet<(usize, SegOutcome)> = JoinSet::new();

    loop {
        if Instant::now() >= throttled_until {
            while set.len() < limit {
                let Some(i) = pending.pop_front() else { break };
                if sh.segments.lock().unwrap()[i].is_done() {
                    continue;
                }
                let (sh2, f) = (sh.clone(), file.clone());
                set.spawn(async move { (i, fetch_segment(&sh2, &f, i).await) });
            }
        }
        if set.is_empty() && pending.is_empty() {
            break;
        }
        let wait_for_throttle = !pending.is_empty() && set.len() < limit;
        tokio::select! {
            Some(joined) = set.join_next() => {
                let (i, outcome) = match joined {
                    Ok(v) => v,
                    Err(e) => return Err(DlError::Failed(e.to_string())),
                };
                match outcome {
                    SegOutcome::Done => limit = (limit + 1).min(seg_count),
                    SegOutcome::Retry { error, progressed } => {
                        if progressed { retries[i] = 0; }
                        retries[i] += 1;
                        if retries[i] > MAX_ATTEMPTS {
                            set.shutdown().await;
                            return Err(DlError::Failed(error));
                        }
                        throttled_until = throttled_until.max(Instant::now() + Duration::from_secs(retries[i].min(10) as u64));
                        if !pending.contains(&i) { pending.push_back(i); }
                    }
                    SegOutcome::Throttle(status) => {
                        limit = limit.min(set.len()).saturating_sub(1).max(1);
                        retries[i] += 1;
                        if retries[i] > MAX_ATTEMPTS {
                            set.shutdown().await;
                            return Err(DlError::Failed(http_status_message(status)));
                        }
                        throttled_until = Instant::now() + Duration::from_secs_f64((1.5 * retries[i] as f64).min(15.0));
                        if !pending.contains(&i) { pending.push_front(i); }
                    }
                    SegOutcome::Fallback => {
                        set.shutdown().await;
                        return Ok(MultiEnd::Fallback);
                    }
                    SegOutcome::Fatal(msg) => {
                        set.shutdown().await;
                        return Err(DlError::Failed(msg));
                    }
                }
            }
            _ = tokio::time::sleep_until(throttled_until.into()), if wait_for_throttle => {}
            else => break,
        }
    }

    let incomplete = sh.segments.lock().unwrap().iter().position(|s| !s.is_done());
    match incomplete {
        None => Ok(MultiEnd::Done),
        Some(i) => Err(DlError::Failed(tf!("Segment {0} could not be completed", i + 1))),
    }
}

async fn fetch_segment(sh: &Shared, file: &File, i: usize) -> SegOutcome {
    let (from, end) = {
        let seg = sh.segments.lock().unwrap()[i];
        (seg.start + seg.received, seg.end)
    };
    if from > end {
        return SegOutcome::Done;
    }
    let url = sh.request_url.lock().unwrap().clone();
    let mut resp = match timeout(REQUEST_TIMEOUT, sh.request(&url, Some(format!("bytes={from}-{end}"))).send()).await {
        Err(_) => return SegOutcome::Retry { error: t("The connection timed out"), progressed: false },
        Ok(Err(e)) => return SegOutcome::Retry { error: error_text(&e), progressed: false },
        Ok(Ok(r)) => r,
    };
    match resp.status().as_u16() {
        206 => {}
        200 => return SegOutcome::Fallback,
        s @ (408 | 425 | 429 | 500 | 502 | 503 | 504) => return SegOutcome::Throttle(s),
        s => return SegOutcome::Fatal(http_status_message(s)),
    }
    // A 206 for a different range would corrupt the file.
    let range_start = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|cr| cr.trim().strip_prefix("bytes").map(str::trim).and_then(|r| r.split('-').next()).and_then(|s| s.trim().parse::<i64>().ok()));
    if range_start.is_some_and(|s| s != from) {
        return SegOutcome::Retry { error: t("Invalid server response"), progressed: false };
    }

    let mut progressed = false;
    loop {
        let chunk = match timeout(READ_TIMEOUT, resp.chunk()).await {
            Err(_) => return SegOutcome::Retry { error: t("The connection timed out"), progressed },
            Ok(Err(e)) => return SegOutcome::Retry { error: error_text(&e), progressed },
            Ok(Ok(None)) => break,
            Ok(Ok(Some(c))) => c,
        };
        let (offset, take) = {
            let seg = sh.segments.lock().unwrap()[i];
            let remaining = seg.length() - seg.received;
            if remaining <= 0 {
                break;
            }
            (seg.start + seg.received, (chunk.len() as i64).min(remaining) as usize)
        };
        if let Err(e) = write_all_at(file, &chunk[..take], offset as u64) {
            return SegOutcome::Fatal(e.to_string());
        }
        sh.segments.lock().unwrap()[i].received += take as i64;
        progressed = true;
        sh.report(false);
        if take < chunk.len() {
            break;
        }
    }
    if sh.segments.lock().unwrap()[i].is_done() {
        SegOutcome::Done
    } else {
        SegOutcome::Retry { error: tf!("Segment {0} could not be completed", i + 1), progressed }
    }
}

// MARK: - Single-stream mode

async fn run_single(sh: &Shared) -> Result<(), DlError> {
    sh.single_mode.store(true, Ordering::Relaxed);
    sh.single_received.store(0, Ordering::Relaxed);
    sh.segments.lock().unwrap().clear();
    let file = open_file(&sh.cfg.part_path, true, None)?;
    let url = sh.request_url.lock().unwrap().clone();
    let mut resp = match timeout(REQUEST_TIMEOUT, sh.request(&url, None).send()).await {
        Err(_) => return Err(DlError::Failed(t("The connection timed out"))),
        Ok(Err(e)) => return Err(DlError::Failed(error_text(&e))),
        Ok(Ok(r)) => r,
    };
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(DlError::Failed(http_status_message(status)));
    }
    // The real GET may differ from the probe (dynamic responses); trust its Content-Length.
    let total = resp.content_length().map(|n| n as i64).unwrap_or(-1);
    let mut received: i64 = 0;
    loop {
        let chunk = match timeout(READ_TIMEOUT, resp.chunk()).await {
            Err(_) => return Err(DlError::Failed(t("The connection timed out"))),
            Ok(Err(e)) => return Err(DlError::Failed(error_text(&e))),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(c))) => c,
        };
        write_all_at(&file, &chunk, received as u64).map_err(|e| DlError::Failed(e.to_string()))?;
        received += chunk.len() as i64;
        sh.single_received.store(received, Ordering::Relaxed);
        sh.report(false);
    }
    if total > 0 && received < total {
        return Err(DlError::Failed(tf!("Connection closed early ({0}/{1} bytes)", received, total)));
    }
    Ok(())
}

// MARK: - Helpers

pub fn http_status_message(status: u16) -> String {
    tf!("Server returned HTTP {0}", status)
}

/// The innermost cause is the readable part ("No such host is known", "connection reset").
pub fn error_text(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return t("The connection timed out");
    }
    let mut source: &dyn std::error::Error = e;
    while let Some(s) = source.source() {
        source = s;
    }
    let text = source.to_string();
    if e.is_connect() {
        format!("{} ({text})", t("Could not connect to the server"))
    } else {
        text
    }
}

pub fn file_name_from_content_disposition(header: &str) -> Option<String> {
    static EXTENDED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)filename\*\s*=\s*[^']*'[^']*'([^;]+)"#).unwrap());
    static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)filename\s*=\s*"([^"]*)""#).unwrap());
    static PLAIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)filename\s*=\s*([^;]+)"#).unwrap());
    if header.trim().is_empty() {
        return None;
    }
    // RFC 5987: filename*=UTF-8''file%20name.zip
    if let Some(c) = EXTENDED.captures(header) {
        let decoded = percent_encoding::percent_decode_str(c[1].trim()).decode_utf8_lossy().trim_matches('"').to_string();
        if !decoded.is_empty() {
            return Some(decoded);
        }
    }
    if let Some(c) = QUOTED.captures(header) {
        let v = c[1].to_string();
        return (!v.is_empty()).then_some(v);
    }
    PLAIN.captures(header).map(|c| c[1].trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition() {
        assert_eq!(file_name_from_content_disposition("attachment; filename=\"a b.zip\"").as_deref(), Some("a b.zip"));
        assert_eq!(file_name_from_content_disposition("attachment; filename*=UTF-8''%C3%A7%C4%B1k.zip").as_deref(), Some("çık.zip"));
        assert_eq!(file_name_from_content_disposition("attachment; filename=plain.bin; size=3").as_deref(), Some("plain.bin"));
        assert_eq!(file_name_from_content_disposition("inline"), None);
    }

    #[test]
    fn segments_cover_the_file() {
        let s = make_segments(1_000_003, 4);
        assert_eq!(s.first().unwrap().start, 0);
        assert_eq!(s.last().unwrap().end, 1_000_002);
        for w in s.windows(2) {
            assert_eq!(w[0].end + 1, w[1].start);
        }
    }
}
