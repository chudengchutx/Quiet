//! 坚果云 WebDAV upload/import: auth beside the library, zip packages off the UI thread.
//!
//! Create an **应用密码** (not the login password) at
//! https://www.jianguoyun.com/d/account#security → 安全选项 → 第三方应用管理.
//! WebDAV endpoint: `https://dav.jianguoyun.com/dav/` with Basic auth
//! (email + 应用密码).

use anyhow::{bail, Context, Result};
use fdl_core::store::image_path;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

const DAV_BASE: &str = "https://dav.jianguoyun.com/dav";

/// Credentials stored as `nutstore-auth.json` next to the document library.
/// Prefer OS keychain later; plaintext file matches the Feishu P0 pattern.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NutstoreAuth {
    #[serde(default)]
    pub email: String,
    /// 应用密码 from 坚果云 → 安全选项 → 第三方应用管理.
    #[serde(default)]
    pub app_password: String,
    /// Remote folder under the WebDAV root (no leading slash). Default `Quiet.md`.
    #[serde(default = "default_remote_root")]
    pub remote_root: String,
}

fn default_remote_root() -> String {
    "Quiet.md".into()
}

impl Default for NutstoreAuth {
    fn default() -> Self {
        Self {
            email: String::new(),
            app_password: String::new(),
            remote_root: default_remote_root(),
        }
    }
}

impl NutstoreAuth {
    pub fn is_ready(&self) -> bool {
        !self.email.trim().is_empty() && !self.app_password.trim().is_empty()
    }

    pub fn remote_root_clean(&self) -> String {
        let r = self.remote_root.trim().trim_matches('/');
        if r.is_empty() {
            default_remote_root()
        } else {
            r.to_string()
        }
    }
}

pub fn auth_path(root: &Path) -> PathBuf {
    root.join("nutstore-auth.json")
}

pub fn load_auth(root: &Path) -> NutstoreAuth {
    let p = auth_path(root);
    std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_auth(root: &Path, auth: &NutstoreAuth) -> Result<()> {
    let p = auth_path(root);
    let bytes = serde_json::to_vec_pretty(auth)?;
    std::fs::write(p, bytes)?;
    Ok(())
}

/// Safe single path segment for WebDAV / zip names.
pub fn sanitize_name(s: &str) -> String {
    let t = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>();
    let t = t.trim().trim_matches('.');
    if t.is_empty() {
        "untitled".into()
    } else {
        // Keep names readable; cap length for URL safety.
        t.chars().take(80).collect()
    }
}

fn percent_encode_path(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len() * 3);
    for b in seg.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b'/' => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn dav_url(remote_path: &str) -> String {
    let path = remote_path.trim_start_matches('/');
    format!("{DAV_BASE}/{}", percent_encode_path(path))
}


/// Ensure each folder segment exists (MKCOL; 405 = already there).
fn ensure_remote_dirs(auth: &NutstoreAuth, folder: &str) -> Result<()> {
    let mut acc = String::new();
    for seg in folder.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        let url = dav_url(&acc);
        let out = Command::new("curl")
            .args([
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--proto",
                "=https",
                "--connect-timeout",
                "20",
                "-u",
                &format!("{}:{}", auth.email.trim(), auth.app_password.trim()),
                "-X",
                "MKCOL",
                &url,
            ])
            .output()
            .context("启动 curl 失败")?;
        let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // 201 created, 405 already exists, 301/302 redirects sometimes.
        if matches!(code.as_str(), "201" | "405" | "301" | "302" | "200") {
            continue;
        }
        if code == "401" || code == "403" {
            bail!("坚果云账号或应用密码不正确，请检查后重试");
        }
        if !out.status.success() && code.is_empty() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("无法创建云端目录: {err}");
        }
        if !matches!(code.as_str(), "201" | "405" | "301" | "302" | "200") {
            bail!("无法创建云端目录（HTTP {code}）");
        }
    }
    Ok(())
}

fn put_file(auth: &NutstoreAuth, remote_path: &str, local: &Path) -> Result<()> {
    let url = dav_url(remote_path);
    let out = Command::new("curl")
        .args([
            "-sS",
            "-f",
            "--proto",
            "=https",
            "--connect-timeout",
            "30",
            "--max-time",
            "600",
            "-u",
            &format!("{}:{}", auth.email.trim(), auth.app_password.trim()),
            "-T",
            local.to_str().context("路径无效")?,
            &url,
        ])
        .output()
        .context("启动 curl 失败")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("401") {
            bail!("坚果云账号或应用密码不正确，请检查后重试");
        }
        bail!("上传失败: {err}");
    }
    Ok(())
}

/// Local files referenced by a document (images, attachments, cover).
fn collect_local_assets(doc: &fdl_core::model::Document) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut push = |src: &str| {
        let Some(p) = image_path(src) else { return };
        if !p.is_file() {
            return;
        }
        if out.iter().any(|(s, _)| s == src) {
            return;
        }
        out.push((src.to_string(), p));
    };
    for src in doc.asset_sources() {
        push(&src);
    }
    if let Some(c) = &doc.cover {
        push(&c.src);
    }
    out
}

fn package_rel_for(path: &Path, assets_dir: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(assets_dir) {
        return format!("assets/{}", rel.to_string_lossy().replace('\\', "/"));
    }
    // Absolute paths outside assets/: keep basename under assets/.
    format!(
        "assets/{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
    )
}

/// Build a zip: `docs/{id}.xml` (paths rewritten to `assets/…`) + asset files.
/// Returns `(zip_path, remote_file_name)`.
pub fn package_doc(
    id: &str,
    title: &str,
    xml: &str,
    doc: &fdl_core::model::Document,
    assets_dir: &Path,
    work: &Path,
) -> Result<(PathBuf, String)> {
    let folder = format!("{}_{}", sanitize_name(title), &id[..id.len().min(8)]);
    let staging = work.join(&folder);
    let docs_dir = staging.join("docs");
    let assets_out = staging.join("assets");
    std::fs::create_dir_all(&docs_dir)?;
    std::fs::create_dir_all(&assets_out)?;

    let assets = collect_local_assets(doc);
    let mut xml_out = xml.to_string();
    for (src, path) in &assets {
        let rel = package_rel_for(path, assets_dir);
        // Copy into staging.
        let dest = staging.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(path, &dest)
            .with_context(|| format!("复制附件失败: {}", path.display()))?;
        if src != &rel {
            xml_out = xml_out.replace(src, &rel);
        }
    }
    std::fs::write(docs_dir.join(format!("{id}.xml")), xml_out.as_bytes())?;

    let zip_name = format!("{folder}.zip");
    let zip_path = work.join(&zip_name);
    if zip_path.exists() {
        let _ = std::fs::remove_file(&zip_path);
    }
    // System zip: recurse from parent so the archive root is `{folder}/…`.
    let status = Command::new("zip")
        .args(["-r", "-q"])
        .arg(&zip_path)
        .arg(&folder)
        .current_dir(work)
        .status()
        .context("启动 zip 失败")?;
    if !status.success() {
        bail!("打包文档失败");
    }
    Ok((zip_path, zip_name))
}

pub type ProgressFn = dyn Fn(&str) + Send + Sync;

fn report(on_progress: Option<&ProgressFn>, msg: &str) {
    if let Some(cb) = on_progress {
        cb(msg);
    }
}

/// Upload one or more documents. Each becomes `{remote_root}/{title}_{id8}.zip`.
pub fn upload_docs(
    ids: &[String],
    titles: &[(String, String)], // (id, title)
    auth: &NutstoreAuth,
    root: &Path,
    on_progress: Option<&ProgressFn>,
) -> Result<usize> {
    if !auth.is_ready() {
        bail!("尚未配置坚果云账号，请先填写邮箱和应用密码");
    }
    let assets_dir = root.join("assets");
    let remote_root = auth.remote_root_clean();
    report(on_progress, "正在连接坚果云…");
    ensure_remote_dirs(auth, &remote_root)?;

    let work = std::env::temp_dir().join(format!(
        "quiet-md-nutstore-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&work)?;
    let mut uploaded = 0usize;
    let total = ids.len();
    let result = (|| -> Result<usize> {
        for (i, id) in ids.iter().enumerate() {
            let title = titles
                .iter()
                .find(|(x, _)| x == id)
                .map(|(_, t)| t.as_str())
                .unwrap_or("untitled");
            report(
                on_progress,
                &format!("正在上传 {}/{}：{title}", i + 1, total),
            );
            let xml_path = root.join("docs").join(format!("{id}.xml"));
            let xml = std::fs::read_to_string(&xml_path)
                .with_context(|| format!("无法读取文档 {id}"))?;
            let doc = fdl_core::docxml::parse(&xml).unwrap_or_default();
            let (zip_path, zip_name) =
                package_doc(id, title, &xml, &doc, &assets_dir, &work)?;
            let remote = format!("{remote_root}/{zip_name}");
            put_file(auth, &remote, &zip_path)?;
            uploaded += 1;
            let _ = std::fs::remove_file(&zip_path);
        }
        Ok(uploaded)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}


/// A `.zip` package listed under the configured remote root.
#[derive(Clone, Debug)]
pub struct RemoteZip {
    pub name: String,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        if b[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Parse PROPFIND multistatus for `.zip` href basenames.
pub fn zip_names_from_propfind(xml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let low = xml.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(z) = low[search_from..].find(".zip") {
        let end = search_from + z + 4;
        let before = &xml[..end];
        let start = before
            .rfind(['/', '>', '"', '\'', ' ', '\t', '\n', '\r'])
            .map(|i| i + 1)
            .unwrap_or(0);
        let raw = &xml[start..end];
        let name = percent_decode(raw);
        let base = name.rsplit('/').next().unwrap_or(&name).to_string();
        if base.to_ascii_lowercase().ends_with(".zip")
            && !base.starts_with('.')
            && !names.iter().any(|n| n == &base)
        {
            names.push(base);
        }
        search_from = end;
    }
    names.sort();
    names
}

/// List `.zip` packages in `{remote_root}/` via WebDAV PROPFIND.
pub fn list_remote_zips(auth: &NutstoreAuth) -> Result<Vec<RemoteZip>> {
    if !auth.is_ready() {
        bail!("尚未配置坚果云账号，请先填写邮箱和应用密码");
    }
    let remote_root = auth.remote_root_clean();
    let url = dav_url(&format!("{remote_root}/"));
    let body = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
        "<d:propfind xmlns:d=\"DAV:\">",
        "<d:prop><d:displayname/><d:getcontentlength/><d:resourcetype/></d:prop>",
        "</d:propfind>",
    );
    let out = Command::new("curl")
        .args([
            "-sS",
            "-f",
            "--proto",
            "=https",
            "--connect-timeout",
            "20",
            "-u",
            &format!("{}:{}", auth.email.trim(), auth.app_password.trim()),
            "-X",
            "PROPFIND",
            "-H",
            "Depth: 1",
            "-H",
            "Content-Type: application/xml; charset=utf-8",
            "-d",
            body,
            &url,
        ])
        .output()
        .context("启动 curl 失败")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("401") {
            bail!("坚果云账号或应用密码不正确，请检查后重试");
        }
        // Empty / missing folder: try MKCOL then empty list, or treat as empty.
        if err.contains("404") {
            return Ok(vec![]);
        }
        bail!("列出坚果云文件失败: {err}");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(zip_names_from_propfind(&text)
        .into_iter()
        .map(|name| RemoteZip { name })
        .collect())
}

fn get_file(auth: &NutstoreAuth, remote_path: &str, local: &Path) -> Result<()> {
    if let Some(parent) = local.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let url = dav_url(remote_path);
    let out = Command::new("curl")
        .args([
            "-sS",
            "-f",
            "--proto",
            "=https",
            "--connect-timeout",
            "30",
            "--max-time",
            "600",
            "-u",
            &format!("{}:{}", auth.email.trim(), auth.app_password.trim()),
            "-o",
            local.to_str().context("路径无效")?,
            &url,
        ])
        .output()
        .context("启动 curl 失败")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("401") {
            bail!("坚果云账号或应用密码不正确，请检查后重试");
        }
        bail!("下载失败: {err}");
    }
    Ok(())
}

fn copy_dir_merge(src: &Path, dest: &Path) -> Result<usize> {
    let mut n = 0usize;
    if !src.is_dir() {
        return Ok(0);
    }
    for entry in walkdir(src)? {
        let rel = entry.strip_prefix(src).unwrap();
        let to = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&to)?;
            continue;
        }
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        if !to.exists() {
            std::fs::copy(&entry, &to)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Recursively list files/dirs under `root` (dirs then files via simple walk).
fn walkdir(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push(p.clone());
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    Ok(out)
}

fn find_named_dirs(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = walkdir(root) else { return found };
    for p in entries {
        if p.is_dir() && p.file_name().and_then(|s| s.to_str()) == Some(name) {
            found.push(p);
        }
    }
    found
}

/// Merge one extracted package into the library. Skips existing `docs/{id}.xml`.
fn merge_extracted(extract_root: &Path, library: &Path) -> Result<(usize, usize)> {
    let mut imported = 0usize;
    let mut skipped = 0usize;
    let docs_dest = library.join("docs");
    let assets_dest = library.join("assets");
    std::fs::create_dir_all(&docs_dest)?;
    std::fs::create_dir_all(&assets_dest)?;

    for docs_dir in find_named_dirs(extract_root, "docs") {
        let Ok(rd) = std::fs::read_dir(&docs_dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("xml") {
                continue;
            }
            let Some(id) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            let dest = docs_dest.join(format!("{id}.xml"));
            if dest.exists() {
                skipped += 1;
                continue;
            }
            std::fs::copy(&p, &dest)
                .with_context(|| format!("写入文档失败: {id}"))?;
            imported += 1;
        }
    }
    for assets_dir in find_named_dirs(extract_root, "assets") {
        let _ = copy_dir_merge(&assets_dir, &assets_dest)?;
    }
    Ok((imported, skipped))
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub zips: usize,
}

/// Download selected remote zips and merge into the local library.
pub fn import_remote_zips(
    names: &[String],
    auth: &NutstoreAuth,
    library: &Path,
    on_progress: Option<&ProgressFn>,
) -> Result<ImportReport> {
    if !auth.is_ready() {
        bail!("尚未配置坚果云账号，请先填写邮箱和应用密码");
    }
    if names.is_empty() {
        bail!("请先选择要导入的文档包");
    }
    let remote_root = auth.remote_root_clean();
    let work = std::env::temp_dir().join(format!(
        "quiet-md-nutstore-import-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&work)?;
    let mut stats = ImportReport {
        zips: names.len(),
        ..Default::default()
    };
    let result = (|| -> Result<ImportReport> {
        for (i, name) in names.iter().enumerate() {
            // Reject path tricks from a tampered listing.
            if name.contains("..") || name.contains('/') || name.contains('\\') {
                bail!("无效的文件名: {name}");
            }
            report(
                on_progress,
                &format!("正在下载 {}/{}：{name}", i + 1, names.len()),
            );
            let zip_path = work.join(name);
            let remote = format!("{remote_root}/{name}");
            get_file(auth, &remote, &zip_path)?;
            let extract_to = work.join(format!("ex-{i}"));
            std::fs::create_dir_all(&extract_to)?;
            report(on_progress, &format!("正在导入 {}/{}…", i + 1, names.len()));
            let status = Command::new("unzip")
                .args(["-q", "-o"])
                .arg(&zip_path)
                .arg("-d")
                .arg(&extract_to)
                .status()
                .context("启动 unzip 失败")?;
            if !status.success() {
                bail!("解压失败: {name}");
            }
            let (imp, skip) = merge_extracted(&extract_to, library)?;
            stats.imported += imp;
            stats.skipped += skip;
            let _ = std::fs::remove_file(&zip_path);
            let _ = std::fs::remove_dir_all(&extract_to);
        }
        Ok(stats)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_bad_chars() {
        assert_eq!(sanitize_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_name("  "), "untitled");
        assert_eq!(sanitize_name("你好文档"), "你好文档");
    }

    #[test]
    fn auth_ready() {
        let mut a = NutstoreAuth::default();
        assert!(!a.is_ready());
        a.email = "a@b.com".into();
        a.app_password = "x".into();
        assert!(a.is_ready());
        assert_eq!(a.remote_root_clean(), "Quiet.md");
    }

    #[test]
    fn parses_propfind_zip_names() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response><D:href>/dav/Quiet.md/</D:href></D:response>
  <D:response><D:href>/dav/Quiet.md/%E5%91%A8%E6%8A%A5_abc12345.zip</D:href></D:response>
  <D:response><D:href>/dav/Quiet.md/note_deadbeef.zip</D:href></D:response>
</D:multistatus>"#;
        let names = zip_names_from_propfind(xml);
        assert_eq!(names, vec!["note_deadbeef.zip".to_string(), "周报_abc12345.zip".to_string()]);
    }
}
