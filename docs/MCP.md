# Quiet.md MCP

本地文档库的 [Model Context Protocol](https://modelcontextprotocol.io/) stdio 服务，方便 Cursor、Claude Desktop 等 AI 工具直接读写 Quiet.md。

实现在 `crates/mcp`（包名 `quiet_md_mcp`，二进制 `quiet-md-mcp`），只依赖 `fdl_core`，不拉 GPUI。文档仍以 DocxXML 存在磁盘上；MCP 工具用 Markdown 导入/导出。

## 构建

在仓库根目录：

```sh
cargo build -p quiet_md_mcp --release
```

二进制：`target/release/quiet-md-mcp`。

开发调试可用：

```sh
cargo run -p quiet_md_mcp -- --home /path/to/temp-library
```

## 文档库路径

与 App 相同，优先级：

1. 命令行 `--home PATH`
2. 环境变量 `QUIET_MD_HOME`
3. 默认 `~/Documents/Quiet.md`

**不要**用 MCP 去改真实用户库做试验；测试请指向临时目录。

## 工具

| 工具 | 作用 |
|---|---|
| `list_documents` | 列出全部文档（id、标题、父级、时间戳、置顶/收藏） |
| `get_document` | 按 id 导出 Markdown |
| `search_documents` | 标题优先、再搜正文（不区分大小写）；`query` 为空则返回最近访问 |
| `create_document` | Markdown 新建；可选 `title`、`parent` |
| `update_document` | Markdown 整篇更新；可选新 `title` |
| `trash_document` | 移入回收站（可在 App 内恢复） |
| `list_folders` | 顶层根节点 + 带有子页面的页 |

新建时：若未传 `title`，用 Markdown 首行 `# 标题`；都没有则标题为空（App 显示「未命名文档」）。

## 接入 Cursor

在 Cursor 的 MCP 设置里加入（把路径换成你的仓库绝对路径）：

```json
{
  "mcpServers": {
    "quiet-md": {
      "command": "/Users/你的用户名/chudeng/quiet-md/target/release/quiet-md-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

只用测试库时：

```json
{
  "mcpServers": {
    "quiet-md": {
      "command": "/Users/你的用户名/chudeng/quiet-md/target/release/quiet-md-mcp",
      "args": ["--home", "/tmp/quiet-md-mcp-demo"],
      "env": {}
    }
  }
}
```

或设 `env.QUIET_MD_HOME`，效果相同。

## 接入 Claude Desktop

编辑 Claude Desktop 的配置文件（macOS 一般在 `~/Library/Application Support/Claude/claude_desktop_config.json`）：

```json
{
  "mcpServers": {
    "quiet-md": {
      "command": "/Users/你的用户名/chudeng/quiet-md/target/release/quiet-md-mcp",
      "args": []
    }
  }
}
```

保存后重启 Claude Desktop。

## 测试

```sh
cargo test -p quiet_md_mcp
```

测试一律在临时目录建库（进程内设 `QUIET_MD_HOME`），不碰 `~/Documents/Quiet.md`。
