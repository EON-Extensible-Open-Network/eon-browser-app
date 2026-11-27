// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! Other browsers' extensions, stage 1 (plan madde 50h, `eon-docs/browser/extensions.md`):
//! installed from a Chrome CRX3, a Firefox XPI, a zip or an unpacked folder; their
//! `declarativeNetRequest` rules run in the shield; their content scripts and styles run
//! in matching pages, with `chrome.runtime` (`id`, `getURL`, `getManifest`) and
//! `chrome.i18n`. Storage is kept in memory for now; messaging and background pages are
//! stage 2.
//!
//! Content scripts share the page's JavaScript world (Servo has no isolated worlds yet).
//! Extension files are served at `https://ext.eon.invalid/<id>/…` only when the
//! extension lists them as web-accessible, so a site cannot probe for installed
//! extensions by their other files.

use std::fs;
use std::path::{Path, PathBuf};

use eon_browser_core::extension::{dnr, package, pattern::Pattern, Manifest};
use eon_browser_core::lists::Lists;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

pub const HOST: &str = "ext.eon.invalid";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    enabled: bool,
    /// `crx3`, `xpi`, `zip` or `folder`.
    source: String,
}

/// One installed extension, read from its folder.
#[derive(Debug, Clone)]
pub struct Installed {
    pub id: String,
    pub enabled: bool,
    pub source: String,
    pub manifest: Manifest,
    pub permissions: Vec<String>,
    /// Rules translated into the shield, and rules that could not be.
    pub rules: usize,
    pub rules_skipped: usize,
    dir: PathBuf,
    raw: Value,
    messages: Value,
}

/// When a content script runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// `document_start` (as soon as the page has a head).
    Start,
    /// `document_end` and `document_idle` (when the page has loaded).
    End,
}

pub struct Extensions {
    root: PathBuf,
    pub list: Vec<Installed>,
}

impl Extensions {
    /// The extensions installed in the data folder.
    pub fn load(data: &Path) -> Extensions {
        let root = data.join("extensions");
        let entries: Vec<Entry> = fs::read(root.join("registry.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let list = entries
            .into_iter()
            .filter_map(|e| read_installed(&root.join(&e.id), e))
            .collect();
        Extensions { root, list }
    }

    fn save(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let entries: Vec<Entry> = self
            .list
            .iter()
            .map(|i| Entry {
                id: i.id.clone(),
                enabled: i.enabled,
                source: i.source.clone(),
            })
            .collect();
        let json = serde_json::to_vec_pretty(&entries).map_err(|e| e.to_string())?;
        fs::write(self.root.join("registry.json"), json).map_err(|e| e.to_string())
    }

    /// Install from a package file (CRX3, XPI, zip) or an unpacked folder. Installing the
    /// same extension again replaces it.
    pub fn install(&mut self, from: &Path, lists: &mut Lists) -> Result<Installed, String> {
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let staging = self.root.join(".installing");
        let _ = fs::remove_dir_all(&staging);
        let source = if from.is_dir() {
            copy_dir(from, &staging)?;
            "folder".to_string()
        } else {
            let bytes = fs::read(from).map_err(|e| e.to_string())?;
            let unpacked = package::unpack(&bytes, &staging)?;
            match unpacked.format {
                package::Format::Crx3 => "crx3",
                package::Format::Xpi => "xpi",
                package::Format::Zip => "zip",
            }
            .to_string()
        };
        let probe = read_installed(
            &staging,
            Entry {
                id: String::new(),
                enabled: true,
                source: source.clone(),
            },
        )
        .ok_or("the package has no readable manifest.json")?;
        let id = slug(&probe.manifest.name);
        let dir = self.root.join(&id);
        let _ = fs::remove_dir_all(&dir);
        fs::rename(&staging, &dir).map_err(|e| e.to_string())?;
        self.list.retain(|i| i.id != id);
        let entry = Entry {
            id: id.clone(),
            enabled: true,
            source,
        };
        let installed =
            read_installed(&dir, entry).ok_or("the installed copy could not be read")?;
        apply_rules(&installed, lists)?;
        self.list.push(installed.clone());
        self.save()?;
        Ok(installed)
    }

    pub fn set_enabled(&mut self, id: &str, on: bool, lists: &mut Lists) -> Result<(), String> {
        let Some(i) = self.list.iter_mut().find(|i| i.id == id) else {
            return Err(format!("no extension {id}"));
        };
        i.enabled = on;
        let copy = i.clone();
        apply_rules(&copy, lists)?;
        self.save()
    }

    pub fn remove(&mut self, id: &str, lists: &mut Lists) -> Result<(), String> {
        self.list.retain(|i| i.id != id);
        lists
            .remove_extension_rules(id)
            .map_err(|e| e.to_string())?;
        let _ = fs::remove_dir_all(self.root.join(id));
        self.save()
    }

    /// The scripts to run in a page at `url` at `stage`, each a self-contained statement.
    pub fn scripts_for(&self, url: &Url, stage: Stage) -> Vec<String> {
        let mut out = Vec::new();
        for ext in self.list.iter().filter(|e| e.enabled) {
            for (n, cs) in ext.manifest.content_scripts.iter().enumerate() {
                let at = match cs.run_at.as_deref() {
                    Some("document_start") => Stage::Start,
                    _ => Stage::End,
                };
                if at != stage {
                    continue;
                }
                let include: Vec<Pattern> = cs
                    .matches
                    .iter()
                    .filter_map(|p| Pattern::parse(p))
                    .collect();
                let exclude: Vec<Pattern> = cs
                    .exclude_matches
                    .iter()
                    .filter_map(|p| Pattern::parse(p))
                    .collect();
                if !eon_browser_core::extension::pattern::any_matches(&include, &exclude, url) {
                    continue;
                }
                let css: String = cs
                    .css
                    .iter()
                    .filter_map(|f| ext.read_text(f))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !css.is_empty() {
                    out.push(crate::browser::inject_css(&css));
                }
                let js: String = cs
                    .js
                    .iter()
                    .filter_map(|f| ext.read_text(f))
                    .collect::<Vec<_>>()
                    .join(";\n");
                if !js.is_empty() {
                    out.push(ext.wrap(n, &js));
                }
            }
        }
        out
    }

    /// A file of an extension, for `https://ext.eon.invalid/<id>/<path>`, if it is
    /// web-accessible.
    pub fn serve(&self, url: &Url) -> Option<(&'static str, Vec<u8>)> {
        let (id, path) = url.path().trim_start_matches('/').split_once('/')?;
        let ext = self.list.iter().find(|e| e.id == id && e.enabled)?;
        if !ext.web_accessible(path) {
            return None;
        }
        let file = safe_join(&ext.dir, path)?;
        let bytes = fs::read(&file).ok()?;
        Some((mime(path), bytes))
    }
}

impl Installed {
    fn read_text(&self, rel: &str) -> Option<String> {
        fs::read_to_string(safe_join(&self.dir, rel)?).ok()
    }

    fn web_accessible(&self, path: &str) -> bool {
        let Some(war) = self
            .raw
            .get("web_accessible_resources")
            .and_then(Value::as_array)
        else {
            return false;
        };
        let globs: Vec<&str> = war
            .iter()
            .flat_map(|w| match w {
                Value::String(s) => vec![s.as_str()],
                Value::Object(o) => o
                    .get("resources")
                    .and_then(Value::as_array)
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default(),
                _ => Vec::new(),
            })
            .collect();
        globs.iter().any(|g| glob(g.trim_start_matches('/'), path))
    }

    /// A content script inside the small `chrome` / `browser` object stage 1 offers. The
    /// guard keeps it from running twice in one document.
    fn wrap(&self, n: usize, code: &str) -> String {
        let id = serde_json::to_string(&self.id).unwrap_or_default();
        let key = serde_json::to_string(&format!("{}|{n}", self.id)).unwrap_or_default();
        let messages = self.messages.to_string();
        let manifest = self.raw.to_string();
        format!(
            "(function(){{\n\
             const __eon=(window.__eonExtensions=window.__eonExtensions||{{}});\n\
             if(__eon[{key}])return;__eon[{key}]=1;\n\
             const __id={id},__msgs={messages},__manifest={manifest};\n\
             const __store=()=>{{const m=new Map();return{{\n\
               get(k,cb){{const o={{}};const ks=k==null?[...m.keys()]:typeof k==='string'?[k]:Array.isArray(k)?k:Object.keys(k);\n\
                 for(const x of ks){{if(m.has(x))o[x]=m.get(x);else if(k&&typeof k==='object'&&!Array.isArray(k))o[x]=k[x];}}\n\
                 if(cb)cb(o);return Promise.resolve(o);}},\n\
               set(o,cb){{for(const x in o)m.set(x,o[x]);if(cb)cb();return Promise.resolve();}},\n\
               remove(k,cb){{for(const x of[].concat(k))m.delete(x);if(cb)cb();return Promise.resolve();}},\n\
               clear(cb){{m.clear();if(cb)cb();return Promise.resolve();}}}};}};\n\
             const __ev=()=>({{addListener(){{}},removeListener(){{}},hasListener(){{return false;}}}});\n\
             const chrome={{\n\
               runtime:{{id:__id,getURL:p=>'https://{HOST}/'+__id+'/'+String(p).replace(/^\\//,''),\n\
                 getManifest:()=>__manifest,sendMessage:()=>Promise.resolve(undefined),onMessage:__ev(),\n\
                 connect:()=>({{postMessage(){{}},onMessage:__ev(),onDisconnect:__ev(),disconnect(){{}}}})}},\n\
               i18n:{{getMessage:(k,s)=>{{let t=((__msgs[k]||__msgs[String(k).toLowerCase()]||{{}}).message||'');\n\
                 [].concat(s||[]).forEach((v,i)=>{{t=t.split('$'+(i+1)).join(String(v));}});return t;}},\n\
                 getUILanguage:()=>navigator.language}},\n\
               storage:{{local:__store(),sync:__store(),session:__store(),onChanged:__ev()}}\n\
             }};\n\
             const browser=chrome;\n\
             try{{\n{code}\n}}catch(e){{}}\n\
             }})();"
        )
    }
}

fn read_installed(dir: &Path, e: Entry) -> Option<Installed> {
    let text = fs::read_to_string(dir.join("manifest.json")).ok()?;
    let raw: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let mut manifest = Manifest::parse(&text).ok()?;
    let mut messages = Value::Object(serde_json::Map::new());
    if let Some(lang) = manifest.default_locale.clone() {
        if let Ok(m) = fs::read_to_string(dir.join("_locales").join(&lang).join("messages.json")) {
            manifest.localize(&m);
            // Keys are case-insensitive: keep a lower-case copy of each.
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&m) {
                let mut all = map.clone();
                for (k, val) in map {
                    all.insert(k.to_lowercase(), val);
                }
                messages = Value::Object(all);
            }
        }
    }
    let (rules, skipped) = rule_lines(dir, &manifest);
    Some(Installed {
        id: e.id,
        enabled: e.enabled,
        source: e.source,
        permissions: manifest.permission_summary(),
        rules: rules.len(),
        rules_skipped: skipped,
        manifest,
        dir: dir.to_path_buf(),
        raw,
        messages,
    })
}

/// The extension's `declarativeNetRequest` rules, translated; and how many were skipped.
fn rule_lines(dir: &Path, m: &Manifest) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut skipped = 0;
    if let Some(dnr_part) = &m.declarative_net_request {
        for r in dnr_part.rule_resources.iter().filter(|r| r.enabled) {
            let Some(path) = safe_join(dir, &r.path) else {
                continue;
            };
            let Ok(json) = fs::read_to_string(path) else {
                continue;
            };
            if let Ok(t) = dnr::translate(&json) {
                lines.extend(t.lines);
                skipped += t.skipped.len();
            }
        }
    }
    (lines, skipped)
}

fn apply_rules(i: &Installed, lists: &mut Lists) -> Result<(), String> {
    let (lines, _) = rule_lines(&i.dir, &i.manifest);
    if i.enabled && !lines.is_empty() {
        lists
            .set_extension_rules(&i.id, &lines)
            .map_err(|e| e.to_string())
    } else {
        lists
            .remove_extension_rules(&i.id)
            .map_err(|e| e.to_string())
    }
}

/// A folder name from an extension's name.
fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() {
        "extension".into()
    } else {
        s.chars().take(60).collect()
    }
}

/// `dir/rel`, only if `rel` stays inside `dir`.
fn safe_join(dir: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = dir.to_path_buf();
    for part in rel.trim_start_matches('/').split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => out.push(p),
        }
    }
    Some(out)
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let p = e.path();
        let target = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &target)?;
        } else {
            fs::copy(&p, &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn glob(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => {
            text.starts_with(head)
                && (0..=text.len() - head.len()).any(|i| {
                    text.is_char_boundary(head.len() + i) && glob(tail, &text[head.len() + i..])
                })
        }
    }
}

fn mime(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "html" | "htm" => "text/html; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_paths() {
        assert_eq!(slug("Dark Reader"), "dark-reader");
        assert_eq!(slug("uBlock Origin Lite!"), "ublock-origin-lite");
        assert_eq!(slug("???"), "extension");
        let d = Path::new("x");
        assert!(safe_join(d, "../evil").is_none());
        assert!(safe_join(d, "C:/evil").is_none());
        assert_eq!(
            safe_join(d, "/img/a.png"),
            Some(PathBuf::from("x/img/a.png"))
        );
    }

    #[test]
    fn web_accessible_globs() {
        assert!(glob("images/*", "images/logo.png"));
        assert!(glob("*.css", "a/b.css"));
        assert!(!glob("images/*", "js/content.js"));
        assert!(glob("exact.png", "exact.png"));
    }
}
