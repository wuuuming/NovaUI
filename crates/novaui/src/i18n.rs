//! Strings by key (`novaui-spec.md`, "The .nui language": `@key`): one table per locale, `key = text`
//! lines, looked up through the active locale, then the fallback locale, then the key itself.
//!
//! ```text
//! # comment
//! settings.title = 设置
//! about.version = 版本 {version}
//! ```
//!
//! `{name}` in a text is replaced by the argument `name` (`{{` and `}}` are literal braces);
//! `\n` is a line break. A `.nui` text written `@settings.title` resolves through `Ui::t`, and
//! a doc presents it again when the locale changes (`Doc::update`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::error::Error;

/// The string tables and the active locale: a handle, cheap to clone, usable from any thread.
/// The process has one ([`global`]: what `Ui::strings`, [`t`] and `.nui` `@key` read), so text
/// made away from the UI (a worker's error) speaks the same language; `Strings::default()` is a
/// table of its own (tests).
#[derive(Clone, Default)]
pub struct Strings(Arc<RwLock<Tables>>);

struct Tables {
    tables: HashMap<String, HashMap<String, String>>,
    locale: String,
    fallback: String,
    /// Keys already reported missing (each is logged once).
    missing: Mutex<HashSet<String>>,
    /// Changes when what a key resolves to changed (a text added, changed or taken away, the
    /// locale switched); adding what is already there leaves it.
    version: u64,
}

impl Default for Tables {
    fn default() -> Self {
        Tables {
            tables: HashMap::new(),
            locale: "en".into(),
            fallback: "en".into(),
            missing: Mutex::new(HashSet::new()),
            version: 0,
        }
    }
}

/// The process's strings.
pub fn global() -> &'static Strings {
    static GLOBAL: OnceLock<Strings> = OnceLock::new();
    GLOBAL.get_or_init(Strings::default)
}

/// `key`'s text from the process's strings ([`Strings::get`]); any thread.
pub fn t(key: &str, args: &[(&str, &str)]) -> String {
    global().get(key, args)
}

impl Strings {
    /// Adds the lines of `source` to `locale`'s table (a key given again takes the new text).
    pub fn add(&self, locale: &str, source: &str) -> Result<(), Error> {
        self.add_under(locale, "", source)
    }

    /// As `add`, each key written `prefix` + key (a plugin's table under `id:`).
    pub fn add_under(&self, locale: &str, prefix: &str, source: &str) -> Result<(), Error> {
        let mut parsed = Vec::new();
        for (n, line) in source.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
                continue;
            }
            let Some((key, text)) = line.split_once('=') else {
                return Err(Error::parse(n + 1, format!("expected `key = text`, found {line:?}")));
            };
            let key = key.trim();
            if key.is_empty() || key.contains(char::is_whitespace) {
                return Err(Error::parse(n + 1, format!("bad key {key:?}")));
            }
            parsed.push((format!("{prefix}{key}"), text.trim().replace("\\n", "\n")));
        }
        self.set(locale, parsed);
        Ok(())
    }

    /// Adds `key → text` pairs to `locale`'s table as they are (no parsing).
    pub fn set(&self, locale: &str, pairs: impl IntoIterator<Item = (String, String)>) {
        let mut t = self.0.write().unwrap();
        let table = t.tables.entry(locale.to_string()).or_default();
        let mut changed = false;
        for (k, v) in pairs {
            changed |= put(table, k, v);
        }
        if changed {
            t.version += 1;
        }
    }

    /// Takes away every key starting with `prefix` (a plugin removed).
    pub fn remove_under(&self, prefix: &str) {
        self.replace_under(prefix, Vec::<(String, Vec<(String, String)>)>::new());
    }

    /// The keys starting with `prefix` become exactly `tables` (locale → `key → text`, each key
    /// written with `prefix`): a plugin's strings read again after an update. The version moves
    /// only if a text changed.
    pub fn replace_under<P>(&self, prefix: &str, tables: impl IntoIterator<Item = (String, P)>)
    where
        P: IntoIterator<Item = (String, String)>,
    {
        let mut fresh: HashMap<String, HashMap<String, String>> = HashMap::new();
        for (locale, pairs) in tables {
            fresh.entry(locale).or_default().extend(pairs.into_iter().map(|(k, v)| (format!("{prefix}{k}"), v)));
        }
        let mut t = self.0.write().unwrap();
        let mut changed = false;
        for (locale, table) in t.tables.iter_mut() {
            let keep = fresh.get(locale);
            let before = table.len();
            table.retain(|k, _| !k.starts_with(prefix) || keep.is_some_and(|f| f.contains_key(k)));
            changed |= table.len() != before;
        }
        for (locale, pairs) in fresh {
            let table = t.tables.entry(locale).or_default();
            for (k, v) in pairs {
                changed |= put(table, k, v);
            }
        }
        if changed {
            t.version += 1;
        }
    }

    /// Switches the active locale (texts bound by key are presented again).
    pub fn set_locale(&self, locale: &str) {
        let mut t = self.0.write().unwrap();
        if switch(&mut t.locale, locale) {
            t.version += 1;
        }
    }

    pub fn locale(&self) -> String {
        self.0.read().unwrap().locale.clone()
    }

    /// The locale a key missing from the active one is looked up in (`en` unless set).
    pub fn set_fallback(&self, locale: &str) {
        let mut t = self.0.write().unwrap();
        if switch(&mut t.fallback, locale) {
            t.version += 1;
        }
    }

    /// The version of what keys resolve to.
    pub fn version(&self) -> u64 {
        self.0.read().unwrap().version
    }

    /// `key`'s text in the active locale, else the fallback's, else the key itself (logged once),
    /// with `{name}` replaced by the matching argument.
    pub fn get(&self, key: &str, args: &[(&str, &str)]) -> String {
        let t = self.0.read().unwrap();
        let text = [&t.locale, &t.fallback].into_iter().find_map(|l| t.tables.get(l)?.get(key));
        match text {
            Some(text) => format(text, args),
            None => {
                if t.missing.lock().unwrap().insert(key.to_string()) {
                    eprintln!("novaui: no string {key:?} in {} or {}", t.locale, t.fallback);
                }
                key.to_string()
            }
        }
    }
}

/// `text` with each `{name}` replaced by its argument; an unknown name stays as written.
fn format(text: &str, args: &[(&str, &str)]) -> String {
    if !text.contains(['{', '}']) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(k) = rest.find(['{', '}']) {
        out.push_str(&rest[..k]);
        let tail = &rest[k..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if tail.starts_with('{') {
            if let Some(end) = tail.find('}') {
                let name = &tail[1..end];
                if let Some((_, v)) = args.iter().find(|(n, _)| *n == name) {
                    out.push_str(v);
                    rest = &tail[end + 1..];
                    continue;
                }
            }
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// Writes text `v` under `k`: a change only when it differs from what is there.
fn put(table: &mut HashMap<String, String>, k: String, v: String) -> bool {
    if table.get(&k) == Some(&v) {
        return false;
    }
    table.insert(k, v);
    true
}

/// Sets a locale field to `to`: a change only when it differs.
fn switch(field: &mut String, to: &str) -> bool {
    if field == to {
        return false;
    }
    *field = to.to_string();
    true
}
