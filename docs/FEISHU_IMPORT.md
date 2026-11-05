# 飞书链接导入（P0）

从飞书 / Lark 的 `/docx/<id>` 链接导入一篇文档到本地库（单向，不是双向同步）。

## 能力

- 解析 `*.feishu.cn` / `*.larksuite.com` 的 docx 链接
- 用 Open API 拉文档 meta + 全部分块，映射进 quiet.md 的 `Document`
- 图片 / 文件按 `file_token` 下载到库的 `assets/`
- 未建模的块类型写成 `BlockKind::Raw`（保留原始 JSON），不丢内容

## 暂不支持

- 知识库 `/wiki/` 链接（请打开文档后复制 `/docx/` 链接）
- 双向同步、评论、权限、电子表格 / 多维表格整表

## 配置

1. 在[飞书开放平台](https://open.feishu.cn/)创建企业自建应用
2. 开通权限（至少）：`docx:document:readonly`、`drive:drive:readonly`（媒体下载）、按需 `offline_access`
3. 在应用里配置桌面端重定向（P0 也可用 `tenant_access_token` 导入应用有权访问的文档）
4. 在 quiet.md 设置中填写 App ID / App Secret，并完成登录（凭证写在文档库旁的 `feishu-auth.json`，勿提交到 git）

## 使用

菜单 **导入为文档…** 旁的 **从飞书链接导入…**，粘贴链接即可。
Put a copy at <library>/feishu-auth.json (see docs/FEISHU_IMPORT.md).
