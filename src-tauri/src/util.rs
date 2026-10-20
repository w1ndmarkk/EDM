use std::ffi::OsStr;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Keeps helper processes (yt-dlp, taskkill, …) from flashing a console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Decimal units like the system file size display ("12 KB", "1.4 MB", "3.27 GB").
pub fn fmt_size(n: i64) -> String {
    if n < 0 {
        return "?".into();
    }
    if n < 1000 {
        return format!("{n} B");
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut v = n as f64 / 1000.0;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let s = match i {
        0 => format!("{v:.0}"),
        1 => format!("{v:.1}"),
        _ => format!("{v:.2}"),
    };
    let s = if crate::i18n::lang() == "tr" { s.replace('.', ",") } else { s };
    format!("{s} {}", units[i])
}

/// A hidden-console `Command`.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Gives a file or folder the Hidden attribute (download temp folders next to the user's files).
pub fn hide(path: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, INVALID_FILE_ATTRIBUTES};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let attrs = GetFileAttributesW(wide.as_ptr());
        if attrs != INVALID_FILE_ATTRIBUTES {
            SetFileAttributesW(wide.as_ptr(), attrs | FILE_ATTRIBUTE_HIDDEN);
        }
    }
}

/// Kills a process and its children (yt-dlp spawns ffmpeg).
pub fn kill_tree(pid: u32) {
    let _ = command("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output();
}

/// Shows a file selected in Explorer (or opens the folder).
pub fn reveal_in_explorer(path: &std::path::Path) {
    if path.is_file() {
        let _ = tauri_plugin_opener::reveal_item_in_dir(path);
    } else {
        let _ = std::fs::create_dir_all(path);
        let _ = tauri_plugin_opener::open_path(path, None::<&str>);
    }
}

/// Opens a file the way a double-click in Explorer does: its associated application, or Windows' "How do you want
/// to open this file?" when there is none (the plain "open" verb fails silently then, e.g. .rar without an
/// archiver). Runs on its own COM thread, as ShellExecuteEx requires.
pub fn open_path(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if !path.is_file() {
        return Err(crate::i18n::t("The file is no longer there."));
    }
    let wide = |p: &std::path::Path| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let file = wide(path);
    let dir = path.parent().map(wide).unwrap_or_else(|| vec![0]);
    std::thread::spawn(move || unsafe {
        CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32);
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC;
        info.lpFile = file.as_ptr();
        info.lpDirectory = dir.as_ptr();
        info.nShow = SW_SHOWNORMAL as i32;
        let ok = ShellExecuteExW(&mut info) != 0;
        let err = std::io::Error::last_os_error();
        CoUninitialize();
        if ok { Ok(()) } else { Err(err.to_string()) }
    })
    .join()
    .unwrap_or_else(|_| Err("open failed".into()))
}

pub fn open_url(url: &str) {
    let _ = tauri_plugin_opener::open_url(url, None::<&str>);
}

/// Case-insensitive wildcard match: `*` = any run of characters, `?` = one character.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let s: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

pub fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
