//! Small modal-style question windows:
//! title, message, up to three buttons and an optional "don't ask again" checkbox.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use serde::Serialize;
use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tokio::sync::oneshot;

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptSpec {
    pub title: String,
    pub message: String,
    /// Monospaced detail text (links, paths).
    pub detail: Option<String>,
    pub buttons: Vec<String>,
    /// Button triggered by Enter.
    pub default_button: usize,
    /// Button triggered by Escape.
    pub cancel_button: Option<usize>,
    pub checkbox: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PromptAnswer {
    /// None when the window was closed without choosing.
    pub button: Option<usize>,
    pub checked: bool,
}

struct Pending {
    spec: PromptSpec,
    reply: Option<oneshot::Sender<PromptAnswer>>,
}

static PENDING: LazyLock<Mutex<HashMap<u64, Pending>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn answer(id: u64, a: PromptAnswer) {
    if let Some(mut p) = PENDING.lock().unwrap().remove(&id) {
        if let Some(tx) = p.reply.take() {
            let _ = tx.send(a);
        }
    }
}

/// Shows the question and waits for the answer. Must be called from an async task.
pub async fn ask(app: &AppHandle, spec: PromptSpec) -> PromptAnswer {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    PENDING.lock().unwrap().insert(id, Pending { spec, reply: Some(tx) });
    let built = crate::windows::painted(WebviewWindowBuilder::new(app, format!("prompt-{id}"), WebviewUrl::App(format!("prompt.html?id={id}").into())))
        .title("EDM")
        .inner_size(500.0, 220.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .always_on_top(true)
        // The page sizes the window to its content, then shows it.
        .visible(false)
        .center()
        .build();
    match built {
        Ok(w) => w.on_window_event(move |e| {
            if matches!(e, WindowEvent::Destroyed) {
                answer(id, PromptAnswer::default());
            }
        }),
        Err(e) => {
            eprintln!("prompt window: {e}");
            answer(id, PromptAnswer::default());
        }
    }
    rx.await.unwrap_or_default()
}

pub fn spec(id: u64) -> Option<PromptSpec> {
    PENDING.lock().unwrap().get(&id).map(|p| p.spec.clone())
}

pub fn reply(id: u64, button: Option<usize>, checked: bool) {
    answer(id, PromptAnswer { button, checked });
}
