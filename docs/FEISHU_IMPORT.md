# 飞书链接导入（P0）

从飞书 / Lark 的 `/docx/<id>` 链接导入一篇文档到本地库（单向，不是双向同步）。

## 能力

- 解析 `*.feishu.cn` / `*.larksuite.com` 的 `/docx/` 与 `/wiki/` 链接
- 用 Open API 拉文档 meta + 全部分块，映射进 quiet.md 的 `Document`
- 文档封面：`GET .../documents/:id` 的 `document.cover`（`token` / `offset_ratio_x` / `offset_ratio_y`）→ quiet.md `Cover`；封面图按媒体 token 下载到 `assets/`
- 文档图标：飞书 Docx OpenAPI 目前不返回标题图标（仅客户端 UI），导入后保持无图标
- 图片 / 文件按 `file_token` 下载到库的 `assets/`
- 内嵌电子表格（`block_type` 30）通过 Sheets API 展开为本地 `table`（有界并行拉取，导入对话框显示阶段进度）
- 未建模的块类型写成带可读标签的 `BlockKind::Raw` 占位（如画板 / 多维表），**不会**把 API JSON 写入文档 XML

## 暂不支持

- 知识库节点若不是 docx（表格 / 多维表等）
- 双向同步、评论、权限；画板 / 多维表格等仍为占位（非整表导入）

## 配置

1. 在[飞书开放平台](https://open.feishu.cn/)创建企业自建应用
2. 开通权限（至少）：`docx:document:readonly`、`drive:drive:readonly`（媒体下载）、`sheets:spreadsheet:readonly`（内嵌表格）、按需 `wiki:wiki:readonly` / `offline_access`
3. 在应用里配置桌面端重定向（P0 也可用 `tenant_access_token` 导入应用有权访问的文档）
4. 在 quiet.md 设置中填写 App ID / App Secret，并完成登录（凭证写在文档库旁的 `feishu-auth.json`，勿提交到 git）

## 使用

菜单 **导入为文档…** 旁的 **从飞书链接导入…**，粘贴链接即可。
Put a copy at <library>/feishu-auth.json (see docs/FEISHU_IMPORT.md).
