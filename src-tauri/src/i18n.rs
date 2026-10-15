//! English is the base language (strings are their own keys); Turkish comes from `ui/i18n/tr.json`,
//! the same table the web UI uses. `{0}`, `{1}`… are positional placeholders.

use std::collections::HashMap;
use std::sync::OnceLock;

static TR_JSON: &str = include_str!("../../ui/i18n/tr.json");

pub fn lang() -> &'static str {
    static LANG: OnceLock<&'static str> = OnceLock::new();
    LANG.get_or_init(|| {
        let tag = std::env::var("EDM_LANG").ok().filter(|s| !s.is_empty()).or_else(sys_locale::get_locale).unwrap_or_default();
        if tag.to_lowercase().starts_with("tr") { "tr" } else { "en" }
    })
}

fn table() -> &'static HashMap<String, String> {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| serde_json::from_str(TR_JSON).unwrap_or_default())
}

pub fn t(key: &str) -> String {
    if lang() == "tr" {
        if let Some(v) = table().get(key) {
            return v.clone();
        }
    }
    key.to_string()
}

pub fn tf(key: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut s = t(key);
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), &a.to_string());
    }
    s
}

/// `tf!("Server returned HTTP {0}", status)`
#[macro_export]
macro_rules! tf {
    ($key:expr $(, $arg:expr)* $(,)?) => {
        $crate::i18n::tf($key, &[$(&$arg as &dyn std::fmt::Display),*])
    };
}
