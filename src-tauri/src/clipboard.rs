//! Clipboard text for the main window. EDM does not watch the clipboard: links that were copied appear in the
//! New Download dialog when it opens (and on Ctrl+V), and nothing happens until the user starts them there.

pub fn read_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

pub fn write_text(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}
