//! Parse Feishu / Lark document links into a document id.

use std::fmt;

/// A Feishu/Lark document the importer can fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRef {
    /// Host the link used (`feishu.cn`, `larksuite.com`, …).
    pub host: String,
    /// `docx` document token / id from `/docx/<id>`.
    pub document_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocUrlError {
    Empty,
    NotHttp,
    NotFeishu,
    /// Wiki / space links need an extra `get_node` step (P1).
    WikiUnsupported,
    NoDocumentId,
}

impl fmt::Display for DocUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocUrlError::Empty => write!(f, "请粘贴飞书文档链接"),
            DocUrlError::NotHttp => write!(f, "链接无效"),
            DocUrlError::NotFeishu => write!(f, "不是飞书 / Lark 文档链接"),
            DocUrlError::WikiUnsupported => write!(f, "暂不支持知识库链接，请打开文档后复制 /docx/ 链接"),
            DocUrlError::NoDocumentId => write!(f, "链接里找不到文档 ID"),
        }
    }
}

impl std::error::Error for DocUrlError {}

fn is_feishu_host(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "feishu.cn"
        || h.ends_with(".feishu.cn")
        || h == "larksuite.com"
        || h.ends_with(".larksuite.com")
        || h == "feishu.net"
        || h.ends_with(".feishu.net")
}

/// Extract a docx document id from a pasted Feishu/Lark URL.
pub fn parse_doc_url(input: &str) -> Result<DocRef, DocUrlError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(DocUrlError::Empty);
    }
    let s = s.trim_matches(|c| c == '\'' || c == '"');
    let lower = s.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(DocUrlError::NotHttp);
    }
    let rest = s.split_once("://").map(|(_, r)| r).ok_or(DocUrlError::NotHttp)?;
    let (host_part, path_q) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host_part.split(':').next().unwrap_or(host_part);
    if !is_feishu_host(host) {
        return Err(DocUrlError::NotFeishu);
    }
    let path = path_q.split('?').next().unwrap_or(path_q).split('#').next().unwrap_or(path_q);
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if let Some(i) = parts.iter().position(|p| *p == "docx") {
        let id = parts.get(i + 1).copied().unwrap_or("").trim();
        if id.is_empty() {
            return Err(DocUrlError::NoDocumentId);
        }
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(DocUrlError::NoDocumentId);
        }
        return Ok(DocRef { host: host.to_ascii_lowercase(), document_id: id.to_string() });
    }
    if parts.iter().any(|p| *p == "wiki" || *p == "space") {
        return Err(DocUrlError::WikiUnsupported);
    }
    Err(DocUrlError::NoDocumentId)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docx_feishu() {
        let r = parse_doc_url("https://my.feishu.cn/docx/ABC123xyz-_").unwrap();
        assert_eq!(r.document_id, "ABC123xyz-_");
        assert_eq!(r.host, "my.feishu.cn");
    }

    #[test]
    fn docx_lark_with_query() {
        let r = parse_doc_url("https://foo.larksuite.com/docx/TokEn99?from=space#block").unwrap();
        assert_eq!(r.document_id, "TokEn99");
    }

    #[test]
    fn rejects_wiki() {
        assert_eq!(parse_doc_url("https://x.feishu.cn/wiki/Abc").unwrap_err(), DocUrlError::WikiUnsupported);
    }

    #[test]
    fn rejects_other() {
        assert_eq!(parse_doc_url("https://example.com/docx/x").unwrap_err(), DocUrlError::NotFeishu);
        assert_eq!(parse_doc_url("").unwrap_err(), DocUrlError::Empty);
    }
}
