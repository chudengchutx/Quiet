//! Feishu link import: OAuth token storage helpers and Open API fetch via `curl`.
//! Mapping lives in `fdl_core::feishu`; this module talks to the network.

use anyhow::{bail, Context, Result};
use fdl_core::feishu::{
    document_from_blocks, parse_doc_url, ApiBlock, DocumentMeta, MapOptions,
};
use fdl_core::feishu::types::{ApiEnvelope, BlocksData, DocumentData};
use fdl_core::model::Document;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const FEISHU_API: &str = "https://open.feishu.cn/open-apis";
const LARK_API: &str = "https://open.larksuite.com/open-apis";

/// Credentials the user configures (App ID / Secret stay out of git).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FeishuAuth {
    #[serde(default)]
    pub app_id: String,
    /// Prefer OS keychain later; plaintext file is opt-in for P0 local use.
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: u64,
}

fn api_base(host: &str) -> &'static str {
    if host.contains("larksuite.com") { LARK_API } else { FEISHU_API }
}

fn curl_json(url: &str, token: &str) -> Result<String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--proto", "=https",
            "--connect-timeout", "20",
            "-H", &format!("Authorization: Bearer {token}"),
            url,
        ])
        .output()
        .context("启动 curl 失败")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("请求失败: {err}");
    }
    Ok(String::from_utf8(out.stdout).context("响应不是 UTF-8")?)
}

fn curl_bytes(url: &str, token: &str) -> Result<Vec<u8>> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--proto", "=https",
            "--connect-timeout", "30",
            "-H", &format!("Authorization: Bearer {token}"),
            url,
        ])
        .output()
        .context("启动 curl 失败")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("下载失败: {err}");
    }
    Ok(out.stdout)
}

/// Fetch document meta + all blocks for a pasted link.
pub fn fetch_document(url: &str, auth: &FeishuAuth) -> Result<(DocumentMeta, Vec<ApiBlock>)> {
    let doc = parse_doc_url(url).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if auth.access_token.is_empty() {
        bail!("尚未登录飞书，请先在设置里完成授权");
    }
    let base = api_base(&doc.host);
    let meta_url = format!("{base}/docx/v1/documents/{}", doc.document_id);
    let meta_body = curl_json(&meta_url, &auth.access_token)?;
    let meta_env: ApiEnvelope<DocumentData> = serde_json::from_str(&meta_body).context("解析文档元数据")?;
    if meta_env.code != 0 {
        bail!("飞书 API: {} ({})", meta_env.msg, meta_env.code);
    }
    let meta = meta_env
        .data
        .and_then(|d| d.document)
        .unwrap_or(DocumentMeta {
            document_id: doc.document_id.clone(),
            revision_id: 0,
            title: String::new(),
        });

    let mut items = Vec::new();
    let mut page_token = String::new();
    loop {
        let mut list_url = format!(
            "{base}/docx/v1/documents/{}/blocks?page_size=500",
            doc.document_id
        );
        if !page_token.is_empty() {
            list_url.push_str(&format!("&page_token={}", urlencoding_lite(&page_token)));
        }
        let body = curl_json(&list_url, &auth.access_token)?;
        // Attach raw_json per item for unknown block fidelity.
        let env: ApiEnvelope<BlocksData> = serde_json::from_str(&body).context("解析 blocks")?;
        if env.code != 0 {
            bail!("飞书 API: {} ({})", env.msg, env.code);
        }
        let data = env.data.unwrap_or(BlocksData { items: vec![], page_token: String::new(), has_more: false });
        // Re-parse items as Value array to keep raw JSON.
        let v: serde_json::Value = serde_json::from_str(&body)?;
        let raw_items = v.pointer("/data/items").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        for (block, raw) in data.items.into_iter().zip(raw_items.into_iter()) {
            let mut block = block;
            block.raw_json = Some(raw.to_string());
            items.push(block);
        }
        if !data.has_more || data.page_token.is_empty() {
            break;
        }
        page_token = data.page_token;
    }
    Ok((meta, items))
}

fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Download media tokens referenced by image/file blocks into `assets_dir`.
pub fn download_media(
    blocks: &[ApiBlock],
    auth: &FeishuAuth,
    host: &str,
    assets_dir: &Path,
) -> Result<HashMap<String, String>> {
    std::fs::create_dir_all(assets_dir)?;
    let base = api_base(host);
    let mut map = HashMap::new();
    let mut tokens = Vec::new();
    for b in blocks {
        if let Some(img) = &b.image {
            if !img.token.is_empty() { tokens.push(img.token.clone()); }
        }
        if let Some(f) = &b.file {
            if !f.token.is_empty() { tokens.push(f.token.clone()); }
        }
    }
    tokens.sort();
    tokens.dedup();
    for token in tokens {
        // Drive media download URL.
        let url = format!("{base}/drive/v1/medias/{token}/download");
        match curl_bytes(&url, &auth.access_token) {
            Ok(bytes) if !bytes.is_empty() => {
                let name = safe_name(&token);
                let path = assets_dir.join(&name);
                std::fs::write(&path, &bytes)?;
                map.insert(token, path.to_string_lossy().to_string());
            }
            Ok(_) => {}
            Err(e) => {
                // Keep importing; media stays as token placeholder.
                eprintln!("feishu media {token}: {e:#}");
            }
        }
    }
    Ok(map)
}

fn safe_name(token: &str) -> String {
    let mut s: String = token.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.len() > 80 { s.truncate(80); }
    if s.is_empty() { s = "media".into(); }
    s
}

/// End-to-end: URL → Document (media downloaded under `assets_dir`).
pub fn import_from_url(url: &str, auth: &FeishuAuth, assets_dir: &Path) -> Result<Document> {
    let doc_ref = parse_doc_url(url).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut auth = auth.clone();
    if auth.access_token.is_empty() {
        if auth.app_id.is_empty() || auth.app_secret.is_empty() {
            bail!("尚未配置飞书应用或登录，请先在文档库的 feishu-auth.json 填写 app_id / app_secret");
        }
        let (token, expire) = fetch_tenant_token(&auth.app_id, &auth.app_secret, &doc_ref.host)?;
        auth.access_token = token;
        auth.expires_at = expire;
    }
    let (meta, blocks) = fetch_document(url, &auth)?;
    let media = download_media(&blocks, &auth, &doc_ref.host, assets_dir)?;
    let opts = MapOptions { media };
    Ok(document_from_blocks(&meta, &blocks, &opts))
}

/// Where we keep Feishu auth beside the library (`feishu-auth.json`).
pub fn auth_path(root: &Path) -> PathBuf {
    root.join("feishu-auth.json")
}

pub fn load_auth(root: &Path) -> FeishuAuth {
    let p = auth_path(root);
    std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_auth(root: &Path, auth: &FeishuAuth) -> Result<()> {
    let p = auth_path(root);
    let bytes = serde_json::to_vec_pretty(auth)?;
    std::fs::write(p, bytes)?;
    Ok(())
}

/// P0 fallback: app `tenant_access_token` (only docs the app can access).
/// User OAuth PKCE is the preferred path; this unblocks import for app-owned docs.
pub fn fetch_tenant_token(app_id: &str, app_secret: &str, host: &str) -> Result<(String, u64)> {
    let base = api_base(host);
    let url = format!("{base}/auth/v3/tenant_access_token/internal");
    let body = serde_json::json!({ "app_id": app_id, "app_secret": app_secret });
    let out = Command::new("curl")
        .args([
            "-fsSL", "--proto", "=https", "--connect-timeout", "20",
            "-H", "Content-Type: application/json",
            "-d", &body.to_string(),
            &url,
        ])
        .output()
        .context("启动 curl 失败")?;
    if !out.status.success() {
        bail!("获取 tenant_access_token 失败");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    #[derive(Deserialize)]
    struct Env { code: i32, #[serde(default)] msg: String, #[serde(default)] tenant_access_token: String, #[serde(default)] expire: u64 }
    // Response is flat: {code, msg, tenant_access_token, expire}
    let env: Env = serde_json::from_str(&text).context("解析 tenant token")?;
    if env.code != 0 { bail!("飞书 API: {} ({})", env.msg, env.code); }
    if env.tenant_access_token.is_empty() { bail!("空的 tenant_access_token"); }
    Ok((env.tenant_access_token, env.expire))
}
