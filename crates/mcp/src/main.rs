//! quiet-md-mcp: stdio MCP server for the Quiet.md document library.

use anyhow::Result;
use quiet_md_mcp::{resolve_root, Library};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::*,
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone)]
struct QuietMcp {
    lib: Arc<Library>,
}

impl QuietMcp {
    fn new(lib: Library) -> Self {
        Self { lib: Arc::new(lib) }
    }

    fn ok(v: serde_json::Value) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&v).unwrap_or_else(|e| e.to_string()),
        )]))
    }

    fn err(e: impl std::fmt::Display) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::error(vec![ContentBlock::text(e.to_string())]))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct IdArgs {
    /// 文档 ID（UUID）
    id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// 搜索关键词；空字符串返回最近访问
    query: String,
    /// 最多返回条数，默认 20
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateArgs {
    /// Markdown 正文；可用首行 `# 标题` 作为文档标题
    markdown: String,
    /// 可选标题（优先于 Markdown 首行一级标题）
    #[serde(default)]
    title: Option<String>,
    /// 可选父文档 ID（挂到文档树下）
    #[serde(default)]
    parent: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateArgs {
    /// 文档 ID
    id: String,
    /// 新的 Markdown 正文（整篇替换）
    markdown: String,
    /// 可选新标题；省略且 Markdown 无一级标题时保留原标题
    #[serde(default)]
    title: Option<String>,
}

#[tool_router]
impl QuietMcp {
    #[tool(description = "列出 Quiet.md 文档库中的全部文档（id、标题、父级、时间戳）")]
    fn list_documents(&self) -> Result<CallToolResult, McpError> {
        match self.lib.list_documents() {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }

    #[tool(description = "按 id 读取一篇文档，返回 Markdown 正文与标题")]
    fn get_document(&self, Parameters(args): Parameters<IdArgs>) -> Result<CallToolResult, McpError> {
        match self.lib.get_document(&args.id) {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }

    #[tool(description = "在文档库中搜索：先匹配标题，再匹配正文（不区分大小写）")]
    fn search_documents(&self, Parameters(args): Parameters<SearchArgs>) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(20) as usize;
        match self.lib.search_documents(&args.query, limit) {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }

    #[tool(description = "用 Markdown 新建一篇文档，可选父文档 id")]
    fn create_document(&self, Parameters(args): Parameters<CreateArgs>) -> Result<CallToolResult, McpError> {
        match self.lib.create_document(&args.markdown, args.title.as_deref(), args.parent.as_deref()) {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }

    #[tool(description = "用 Markdown 整篇更新已有文档（可改标题）")]
    fn update_document(&self, Parameters(args): Parameters<UpdateArgs>) -> Result<CallToolResult, McpError> {
        match self.lib.update_document(&args.id, &args.markdown, args.title.as_deref()) {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }

    #[tool(description = "将文档移入回收站（可从 Quiet.md 应用内恢复）")]
    fn trash_document(&self, Parameters(args): Parameters<IdArgs>) -> Result<CallToolResult, McpError> {
        match self.lib.trash_document(&args.id) {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }

    #[tool(description = "列出文档树：顶层根节点，以及带有子页面的「文件夹」页")]
    fn list_folders(&self) -> Result<CallToolResult, McpError> {
        match self.lib.list_folders() {
            Ok(v) => Self::ok(v),
            Err(e) => Self::err(e),
        }
    }
}

#[tool_handler(
    name = "quiet-md",
    version = "0.1.4",
    instructions = "Quiet.md 本地文档库 MCP。文档以 DocxXML 存在磁盘，本服务用 Markdown 读写。工具：list_documents / get_document / search_documents / create_document / update_document / trash_document / list_folders。默认库路径与 App 相同：~/Documents/Quiet.md，可用 QUIET_MD_HOME 或 --home 覆盖。"
)]
impl ServerHandler for QuietMcp {}

#[tokio::main]
async fn main() -> Result<()> {
    // MCP 协议走 stdout；日志只能写 stderr。
    let root = resolve_root(std::env::args().skip(1))?;
    // 让 Store::root_dir / image_path 与 open_at 指向同一库。
    unsafe { std::env::set_var("QUIET_MD_HOME", &root); }
    let lib = Library::open(root)?;
    eprintln!("quiet-md-mcp: library {}", lib.root().display());

    let server = QuietMcp::new(lib).serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}
