//! Quiet.md library helpers used by the MCP stdio server.
//! Opens a Store under a given root (tests use a temp dir; the binary uses
//! `QUIET_MD_HOME` or `~/Documents/Quiet.md`).

use anyhow::{bail, Context, Result};
use fdl_core::markdown::{self, Options as MdOptions};
use fdl_core::model::{display_title, Block, BlockKind, Document, RichText};
use fdl_core::paste::{self, Off};
use fdl_core::store::{Entry, Store};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// One document library root.
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn open(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("docs"))
            .with_context(|| format!("create library at {}", root.display()))?;
        let _ = Store::open_at(root.clone())?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn store(&self) -> Result<Store> {
        Store::open_at(self.root.clone())
    }

    /// All documents: id, title, parent, timestamps, flags.
    pub fn list_documents(&self) -> Result<Value> {
        let mut s = self.store()?;
        let mut entries = s.list();
        entries.sort_by(|a, b| b.meta.modified.cmp(&a.meta.modified));
        Ok(json!({
            "root": self.root.display().to_string(),
            "documents": entries.iter().map(entry_json).collect::<Vec<_>>(),
        }))
    }

    /// Export one document as Markdown (text only; no image assets).
    pub fn get_document(&self, id: &str) -> Result<Value> {
        let mut s = self.store()?;
        let doc = s.load(id).with_context(|| format!("load document {id}"))?;
        let md = markdown::export(&doc, &MdOptions::default()).text;
        let parent = s.list().into_iter().find(|e| e.id == id).and_then(|e| e.meta.parent);
        Ok(json!({
            "id": id,
            "title": display_title(&doc.title.text),
            "icon": doc.icon,
            "markdown": md,
            "parent": parent,
        }))
    }

    /// Case-insensitive search over title then body (same order as the app).
    pub fn search_documents(&self, query: &str, limit: usize) -> Result<Value> {
        let q = query.trim().to_lowercase();
        let mut s = self.store()?;
        let entries = s.list();
        let mut title_hits = Vec::new();
        let mut body_hits = Vec::new();
        if q.is_empty() {
            let mut recent = entries;
            recent.sort_by(|a, b| {
                let ka = a.meta.opened.max(a.meta.modified);
                let kb = b.meta.opened.max(b.meta.modified);
                kb.cmp(&ka)
            });
            for e in recent.into_iter().take(limit.max(1)) {
                title_hits.push(search_hit(&e, "recent", None));
            }
        } else {
            for e in &entries {
                if e.title.to_lowercase().contains(&q) {
                    title_hits.push(search_hit(e, "title", snippet(&e.title, &q)));
                } else if e.body.to_lowercase().contains(&q) {
                    body_hits.push(search_hit(e, "body", snippet(&e.body, &q)));
                }
            }
        }
        let mut hits = title_hits;
        hits.append(&mut body_hits);
        hits.truncate(limit.max(1));
        Ok(json!({ "query": query, "hits": hits }))
    }

    /// Create a document from Markdown. Title comes from `title`, else the
    /// leading `# heading`, else empty (UI shows 未命名文档).
    pub fn create_document(&self, markdown: &str, title: Option<&str>, parent: Option<&str>) -> Result<Value> {
        let doc = document_from_markdown(markdown, title)?;
        let mut s = self.store()?;
        let id = s.create_from(&doc)?;
        if let Some(p) = parent.filter(|p| !p.is_empty()) {
            s.set_parent(&id, Some(p))?;
        }
        Ok(json!({
            "id": id,
            "title": display_title(&doc.title.text),
            "parent": parent.filter(|p| !p.is_empty()),
        }))
    }

    /// Replace a document's body (and optional title) from Markdown.
    pub fn update_document(&self, id: &str, markdown: &str, title: Option<&str>) -> Result<Value> {
        let mut s = self.store()?;
        let existing = s.load(id).with_context(|| format!("load document {id}"))?;
        let mut doc = document_from_markdown(markdown, title)?;
        if title.is_none() && doc.title.text.trim().is_empty() {
            doc.title = existing.title;
        }
        if doc.icon.is_none() {
            doc.icon = existing.icon;
        }
        if doc.cover.is_none() {
            doc.cover = existing.cover;
        }
        s.save(id, &doc)?;
        Ok(json!({
            "id": id,
            "title": display_title(&doc.title.text),
        }))
    }

    /// Move a document into the 回收站.
    pub fn trash_document(&self, id: &str) -> Result<Value> {
        let mut s = self.store()?;
        s.delete(id, None)?;
        Ok(json!({ "id": id, "trashed": true }))
    }

    /// Pages that have children, plus top-level roots (parent tree).
    pub fn list_folders(&self) -> Result<Value> {
        let mut s = self.store()?;
        let entries = s.list();
        let ids: std::collections::HashSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        let mut kids: std::collections::HashMap<&str, Vec<&Entry>> = std::collections::HashMap::new();
        let mut roots = Vec::new();
        for e in &entries {
            match e.meta.parent.as_deref().filter(|p| ids.contains(p) && *p != e.id.as_str()) {
                Some(p) => kids.entry(p).or_default().push(e),
                None => roots.push(e),
            }
        }
        for v in kids.values_mut() {
            v.sort_by_key(|e| e.meta.sibling_key());
        }
        roots.sort_by_key(|e| e.meta.sibling_key());
        let folder = |e: &Entry| {
            let children = kids.get(e.id.as_str()).map(|v| v.len()).unwrap_or(0);
            json!({
                "id": e.id,
                "title": display_title(&e.title),
                "parent": e.meta.parent,
                "child_count": children,
            })
        };
        let mut folders: Vec<Value> = entries.iter()
            .filter(|e| kids.contains_key(e.id.as_str()))
            .map(|e| folder(e))
            .collect();
        folders.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
        Ok(json!({
            "roots": roots.iter().map(|e| folder(e)).collect::<Vec<_>>(),
            "folders": folders,
        }))
    }
}

fn entry_json(e: &Entry) -> Value {
    json!({
        "id": e.id,
        "title": display_title(&e.title),
        "icon": e.icon,
        "parent": e.meta.parent,
        "created": e.meta.created,
        "modified": e.meta.modified,
        "opened": e.meta.opened,
        "pinned": e.meta.pinned,
        "starred": e.meta.starred > 0,
    })
}

#[derive(Serialize)]
struct Hit<'a> {
    id: &'a str,
    title: String,
    r#match: &'a str,
    snippet: Option<String>,
}

fn search_hit<'a>(e: &'a Entry, kind: &'a str, snippet: Option<String>) -> Value {
    serde_json::to_value(Hit { id: &e.id, title: display_title(&e.title), r#match: kind, snippet }).unwrap()
}

fn snippet(hay: &str, needle_lower: &str) -> Option<String> {
    let lower = hay.to_lowercase();
    let i = lower.find(needle_lower)?;
    let line_start = hay[..i].rfind('\n').map(|j| j + 1).unwrap_or(0);
    let start = if i > line_start + 40 { i - 40 } else { line_start };
    let start = hay.floor_char_boundary(start);
    let end = hay.ceil_char_boundary((i + needle_lower.len() + 80).min(hay.len()));
    let mut s = String::new();
    if start > line_start { s.push('…'); }
    s.push_str(hay[start..end].trim());
    if end < hay.len() { s.push('…'); }
    Some(s)
}

/// Markdown → Document, matching the app's file import rules.
pub fn document_from_markdown(markdown: &str, title: Option<&str>) -> Result<Document> {
    let text = markdown.trim_start_matches('\u{feff}');
    let off = Off { file: true, ..Default::default() };
    let mut blocks = paste::markdown_to_blocks(text, off);
    let mut doc_title = title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or("").to_string();
    if doc_title.is_empty() {
        if let Some(b) = blocks.first() {
            if matches!(b.kind, BlockKind::Heading { level: 1, .. }) {
                doc_title = b.text.text.trim().to_string();
                blocks.remove(0);
            }
        }
    } else if blocks.first().is_some_and(|b| {
        matches!(b.kind, BlockKind::Heading { level: 1, .. }) && b.text.text.trim() == doc_title
    }) {
        blocks.remove(0);
    }
    if blocks.is_empty() {
        blocks.push(Block::text(""));
    }
    Ok(Document { title: RichText::plain(doc_title), icon: None, cover: None, blocks })
}

/// Resolve the library root: `--home PATH`, else `QUIET_MD_HOME`, else the
/// same Documents path the app uses.
pub fn resolve_root(args: impl IntoIterator<Item = String>) -> Result<PathBuf> {
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--home" => {
                let p = args.next().context("--home needs a path")?;
                return Ok(PathBuf::from(p));
            }
            "-h" | "--help" => bail!("usage: quiet-md-mcp [--home PATH]"),
            other if other.starts_with('-') => bail!("unknown flag: {other}"),
            other => return Ok(PathBuf::from(other)),
        }
    }
    Ok(Store::root_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("quiet-md-mcp-{name}-{}", stamp()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            unsafe { std::env::set_var("QUIET_MD_HOME", &d); }
            Tmp(d)
        }
        fn lib(&self) -> Library {
            Library::open(self.0.clone()).unwrap()
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn stamp() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        format!("{n:x}")
    }

    #[test]
    fn create_get_search_update_trash() {
        let tmp = Tmp::new("roundtrip");
        let lib = tmp.lib();
        let created = lib.create_document("# 你好\n\n世界正文搜索词\n", None, None).unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["title"], "你好");

        let got = lib.get_document(&id).unwrap();
        assert!(got["markdown"].as_str().unwrap().contains("世界正文搜索词"));
        assert_eq!(got["title"], "你好");

        let hits = lib.search_documents("搜索词", 10).unwrap();
        assert_eq!(hits["hits"].as_array().unwrap().len(), 1);
        assert_eq!(hits["hits"][0]["id"], id);

        lib.update_document(&id, "# 你好\n\n更新后的内容\n", Some("你好")).unwrap();
        let got = lib.get_document(&id).unwrap();
        assert!(got["markdown"].as_str().unwrap().contains("更新后的内容"));

        let list = lib.list_documents().unwrap();
        assert_eq!(list["documents"].as_array().unwrap().len(), 1);

        lib.trash_document(&id).unwrap();
        let list = lib.list_documents().unwrap();
        assert!(list["documents"].as_array().unwrap().is_empty());
    }

    #[test]
    fn parent_and_folders() {
        let tmp = Tmp::new("folders");
        let lib = tmp.lib();
        let parent = lib.create_document("# 父页\n\n", None, None).unwrap();
        let pid = parent["id"].as_str().unwrap().to_string();
        let child = lib.create_document("# 子页\n\n", None, Some(&pid)).unwrap();
        assert_eq!(child["parent"], pid);
        let folders = lib.list_folders().unwrap();
        assert_eq!(folders["folders"].as_array().unwrap().len(), 1);
        assert_eq!(folders["folders"][0]["id"], pid);
        assert_eq!(folders["folders"][0]["child_count"], 1);
    }

    #[test]
    fn resolve_home_flag() {
        let p = resolve_root(["--home".into(), "/tmp/qmd-test".into()]).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/qmd-test"));
    }
}
