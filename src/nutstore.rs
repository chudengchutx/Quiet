//! 坚果云 WebDAV upload: auth beside the library, zip packages off the UI thread.
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
}
