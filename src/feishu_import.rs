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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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


/// Optional progress callback for the import dialog (Chinese stage lines).
pub type ProgressFn = dyn Fn(&str) + Send + Sync;

fn report(on_progress: Option<&ProgressFn>, msg: &str) {
    if let Some(cb) = on_progress {
        cb(msg);
    }
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

/// Resolve a wiki node token to a docx `document_id` via `wiki/v2/spaces/get_node`.
fn resolve_wiki_document_id(base: &str, wiki_token: &str, auth: &FeishuAuth) -> Result<String> {
    let url = format!(
        "{base}/wiki/v2/spaces/get_node?token={}",
        urlencoding_lite(wiki_token)
    );
    let body = curl_json(&url, &auth.access_token)?;
    let v: serde_json::Value = serde_json::from_str(&body).context("解析知识库节点")?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("unknown");
        bail!("飞书 API: {msg} ({code})");
    }
    let node = v.pointer("/data/node").context("知识库节点响应缺少 data.node")?;
    let obj_type = node.get("obj_type").and_then(|t| t.as_str()).unwrap_or("");
    if obj_type != "docx" && obj_type != "doc" {
        bail!("{}", fdl_core::feishu::DocUrlError::WikiNotDocx);
    }
    let obj_token = node
        .get("obj_token")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();
    if obj_token.is_empty() {
        bail!("知识库节点没有文档 token");
    }
    Ok(obj_token.to_string())
}

/// Fetch document meta + all blocks for a pasted link.
pub fn fetch_document(
    url: &str,
    auth: &FeishuAuth,
    on_progress: Option<&ProgressFn>,
) -> Result<(DocumentMeta, Vec<ApiBlock>)> {
    let mut doc = parse_doc_url(url).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if auth.access_token.is_empty() {
        bail!("尚未登录飞书，请先在设置里完成授权");
    }
    let base = api_base(&doc.host);
    if let Some(wiki) = doc.wiki_token.clone() {
        report(on_progress, "正在解析知识库链接…");
        doc.document_id = resolve_wiki_document_id(base, &wiki, auth)?;
        doc.wiki_token = None;
    }
    report(on_progress, "正在拉取文档内容…");
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

/// Ensure `access_token` is set (tenant-token fallback, same as import).
pub fn prepare_auth(auth: &FeishuAuth, host: &str) -> Result<FeishuAuth> {
    let mut auth = auth.clone();
    if auth.access_token.is_empty() {
        if auth.app_id.is_empty() || auth.app_secret.is_empty() {
            bail!("尚未配置飞书应用或登录，请先在文档库的 feishu-auth.json 填写 app_id / app_secret");
        }
        let (token, expire) = fetch_tenant_token(&auth.app_id, &auth.app_secret, host)?;
        auth.access_token = token;
        auth.expires_at = expire;
    }
    Ok(auth)
}

/// Download one Drive media token into `assets_dir`. Returns the absolute path.
pub fn download_media_token(
    token: &str,
    auth: &FeishuAuth,
    host: &str,
    assets_dir: &Path,
) -> Result<PathBuf> {
    let token = token.trim();
    if token.is_empty() {
        bail!("空的媒体 token");
    }
    if auth.access_token.is_empty() {
        bail!("尚未登录飞书，请先在设置里完成授权");
    }
    std::fs::create_dir_all(assets_dir)?;
    let base = api_base(host);
    let url = format!("{base}/drive/v1/medias/{token}/download");
    let bytes = curl_bytes(&url, &auth.access_token)?;
    if bytes.is_empty() {
        bail!("下载内容为空");
    }
    let name = safe_name(token);
    let path = assets_dir.join(&name);
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

/// Download media tokens referenced by image/file blocks into `assets_dir`.
pub fn download_media(
    blocks: &[ApiBlock],
    auth: &FeishuAuth,
    host: &str,
    assets_dir: &Path,
) -> Result<HashMap<String, String>> {
    std::fs::create_dir_all(assets_dir)?;
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
        match download_media_token(&token, auth, host, assets_dir) {
            Ok(path) => {
                map.insert(token, path.to_string_lossy().to_string());
            }
            Err(e) => {
                // Keep importing; media stays as token placeholder.
                eprintln!("feishu media {token}: {e:#}");
            }
        }
    }
    Ok(map)
}

/// Strip a `feishu-token:…` image src to the Drive file token, if present.
pub fn feishu_media_token(src: &str) -> Option<&str> {
    src.strip_prefix("feishu-token:")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn safe_name(token: &str) -> String {
    let mut s: String = token.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.len() > 80 { s.truncate(80); }
    if s.is_empty() { s = "media".into(); }
    s
}

/// Split sheet block token `spreadsheetToken_sheetId` on the **last** `_`.
fn split_sheet_token(token: &str) -> Option<(&str, &str)> {
    let idx = token.rfind('_')?;
    let spreadsheet = &token[..idx];
    let sheet_id = &token[idx + 1..];
    if spreadsheet.is_empty() || sheet_id.is_empty() {
        return None;
    }
    Some((spreadsheet, sheet_id))
}

/// Convert a Sheets API cell value into a display string.
fn cell_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => {
            if *b {
                "TRUE".into()
            } else {
                "FALSE".into()
            }
        }
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .map(cell_to_string)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(""),
        serde_json::Value::Object(map) => {
            for key in ["text", "content", "name", "title", "link"] {
                if let Some(inner) = map.get(key) {
                    let s = cell_to_string(inner);
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
            // Mention / rich cells: flatten string leaves.
            let mut parts = Vec::new();
            collect_string_leaves(v, &mut parts);
            parts.join("")
        }
    }
}

fn collect_string_leaves(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) if !s.is_empty() => out.push(s.clone()),
        serde_json::Value::Array(arr) => {
            for x in arr {
                collect_string_leaves(x, out);
            }
        }
        serde_json::Value::Object(map) => {
            for x in map.values() {
                collect_string_leaves(x, out);
            }
        }
        _ => {}
    }
}

fn parse_sheet_values(body: &str) -> Result<Vec<Vec<String>>> {
    let v: serde_json::Value = serde_json::from_str(body).context("解析电子表格响应")?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("unknown");
        bail!("飞书 Sheets API: {msg} ({code})");
    }
    let values = v
        .pointer("/data/valueRange/values")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let mut rows = Vec::with_capacity(values.len());
    for row in values {
        let cells = match row {
            serde_json::Value::Array(arr) => arr.iter().map(cell_to_string).collect(),
            other => vec![cell_to_string(&other)],
        };
        rows.push(cells);
    }
    Ok(rows)
}

/// Fetch used-range values for every embedded sheet block. Failures are logged and skipped.
/// Sheet requests run with bounded concurrency (std threads, no extra deps).
fn fetch_sheet_values(
    blocks: &[ApiBlock],
    auth: &FeishuAuth,
    host: &str,
    on_progress: Option<&ProgressFn>,
) -> HashMap<String, Vec<Vec<String>>> {
    let base = api_base(host);
    let mut jobs: Vec<(String, String)> = Vec::new();
    for b in blocks {
        if b.block_type != 30 {
            continue;
        }
        let Some(sheet) = &b.sheet else { continue };
        if sheet.token.is_empty() {
            continue;
        }
        let Some((spreadsheet_token, sheet_id)) = split_sheet_token(&sheet.token) else {
            eprintln!("feishu sheet {}: malformed token (no last `_`)", b.block_id);
            continue;
        };
        let url = format!(
            "{base}/sheets/v2/spreadsheets/{spreadsheet_token}/values/{}?valueRenderOption=FormattedValue&dateTimeRenderOption=FormattedString",
            urlencoding_lite(sheet_id)
        );
        jobs.push((b.block_id.clone(), url));
    }
    let n = jobs.len();
    if n == 0 {
        return HashMap::new();
    }
    report(
        on_progress,
        &crate::localized_format!("正在拉取表格 0/{}…", "Fetching sheet 0/{}…", n),
    );

    const WORKERS: usize = 5;
    let token = auth.access_token.clone();
    let queue: Mutex<std::vec::IntoIter<(String, String)>> = Mutex::new(jobs.into_iter());
    let out: Mutex<HashMap<String, Vec<Vec<String>>>> = Mutex::new(HashMap::new());
    let done = AtomicUsize::new(0);
    let workers = WORKERS.min(n).max(1);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = &queue;
            let out = &out;
            let done = &done;
            let token = &token;
            let on_progress = &on_progress;
            scope.spawn(move || {
                loop {
                    let next = {
                        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                        q.next()
                    };
                    let Some((block_id, url)) = next else { break };
                    match curl_json(&url, token) {
                        Ok(body) => match parse_sheet_values(&body) {
                            Ok(rows) => {
                                out.lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(block_id.clone(), rows);
                            }
                            Err(e) => eprintln!("feishu sheet {block_id}: {e:#}"),
                        },
                        Err(e) => eprintln!("feishu sheet {block_id}: {e:#}"),
                    }
                    let i = done.fetch_add(1, Ordering::Relaxed) + 1;
                    report(
                        *on_progress,
                        &crate::localized_format!(
                            "正在拉取表格 {}/{}…",
                            "Fetching sheet {}/{}…",
                            i,
                            n
                        ),
                    );
                }
            });
        }
    });

    out.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// End-to-end: URL → Document (media downloaded under `assets_dir`).
/// `on_progress` receives short Chinese stage lines for the import dialog.
pub fn import_from_url(
    url: &str,
    auth: &FeishuAuth,
    assets_dir: &Path,
    on_progress: Option<&ProgressFn>,
) -> Result<Document> {
    let doc_ref = parse_doc_url(url).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if auth.access_token.is_empty() {
        report(on_progress, "正在获取访问令牌…");
    }
    let auth = prepare_auth(auth, &doc_ref.host)?;
    let (meta, blocks) = fetch_document(url, &auth, on_progress)?;
    let media_n = {
        let mut tokens = Vec::new();
        for b in &blocks {
            if let Some(img) = &b.image {
                if !img.token.is_empty() {
                    tokens.push(img.token.as_str());
                }
            }
            if let Some(f) = &b.file {
                if !f.token.is_empty() {
                    tokens.push(f.token.as_str());
                }
            }
        }
        tokens.sort();
        tokens.dedup();
        tokens.len()
    };
    if media_n > 0 {
        report(
            on_progress,
            &crate::localized_format!(
                "正在下载图片与附件（{}）…",
                "Downloading images and attachments ({})…",
                media_n
            ),
        );
    }
    let media = download_media(&blocks, &auth, &doc_ref.host, assets_dir)?;
    let sheets = fetch_sheet_values(&blocks, &auth, &doc_ref.host, on_progress);
    report(on_progress, "正在整理文档…");
    let opts = MapOptions { media, sheets };
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

#[cfg(test)]
mod tests {
    use super::feishu_media_token;

    #[test]
    fn parses_feishu_token_src() {
        assert_eq!(feishu_media_token("feishu-token:abc123"), Some("abc123"));
        assert_eq!(feishu_media_token("feishu-token:  tok  "), Some("tok"));
        assert_eq!(feishu_media_token("feishu-token:"), None);
        assert_eq!(feishu_media_token("https://example.com/a.png"), None);
        assert_eq!(feishu_media_token("/abs/path.png"), None);
    }
}
