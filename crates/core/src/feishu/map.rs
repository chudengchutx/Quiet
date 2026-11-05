//! Map Feishu Open API blocks into quiet.md `Document` / `Block`.

use crate::model::{
    Align, Block, BlockKind, ColorName, Cover, Document, FileBlock, FileView, Inline, InlineDocLink,
    InlineKind, InlineReminder, Marks, RichText, Table, TableCell,
};
use crate::feishu::types::{
    ApiBlock, DocumentMeta, MentionDoc, TextContainer, TextElement, TextElementStyle,
};
use serde_json::Value;
use std::collections::HashMap;

/// How media tokens become local paths. The app fills this after download.
#[derive(Clone, Debug, Default)]
pub struct MapOptions {
    /// `file_token` → absolute path (or `assets/...`) for images and files.
    pub media: HashMap<String, String>,
    /// Prefetched sheet values keyed by Feishu `block_id` (rows of display strings).
    pub sheets: HashMap<String, Vec<Vec<String>>>,
}

/// Build a `Document` from the document meta and the flat blocks list.
pub fn document_from_blocks(meta: &DocumentMeta, blocks: &[ApiBlock], opts: &MapOptions) -> Document {
    let by_id: HashMap<&str, &ApiBlock> = blocks.iter().map(|b| (b.block_id.as_str(), b)).collect();
    let root_id = blocks
        .iter()
        .find(|b| b.block_type == 1)
        .map(|b| b.block_id.as_str())
        .or_else(|| blocks.first().map(|b| b.block_id.as_str()))
        .unwrap_or("");

    let title = if !meta.title.is_empty() {
        RichText::plain(&meta.title)
    } else if let Some(page) = by_id.get(root_id).and_then(|b| b.page.as_ref()) {
        rich_from_elements(&page.elements)
    } else {
        RichText::plain("")
    };

    let child_ids = by_id.get(root_id).map(|b| b.children.as_slice()).unwrap_or(&[]);
    let mut out = Vec::new();
    for id in child_ids {
        if let Some(b) = by_id.get(id.as_str()) {
            out.extend(map_block(b, &by_id, opts));
        }
    }
    if out.is_empty() {
        // Flat fallback: every top-level-looking block that isn't the page.
        for b in blocks {
            if b.block_id == root_id || b.block_type == 1 {
                continue;
            }
            if b.parent_id.is_empty() || b.parent_id == root_id {
                out.extend(map_block(b, &by_id, opts));
            }
        }
    }
    if out.is_empty() {
        out.push(Block::text(""));
    }
    // Title icon: Feishu Docx OpenAPI has no documented field (UI-only today).
    let icon = None;
    let cover = meta.cover.as_ref().and_then(|c| map_cover(c, opts));
    Document { title, icon, cover, blocks: out }
}

/// Map Feishu `document.cover` into quiet.md `Cover` when the media token resolved.
/// Missing token / failed download → `None` (soft-fail; import still succeeds).
fn map_cover(c: &crate::feishu::types::DocumentCover, opts: &MapOptions) -> Option<Cover> {
    let token = c.token.trim();
    if token.is_empty() {
        return None;
    }
    let src = opts.media.get(token).cloned()?;
    Some(Cover {
        src,
        x: c.offset_ratio_x,
        y: c.offset_ratio_y,
    })
}

fn map_block(b: &ApiBlock, by_id: &HashMap<&str, &ApiBlock>, opts: &MapOptions) -> Vec<Block> {
    match b.block_type {
        1 => Vec::new(), // page
        2 => vec![textish(BlockKind::Text, b.text.as_ref(), b, by_id, opts)],
        3 => vec![heading(1, b.heading1.as_ref(), b, by_id, opts)],
        4 => vec![heading(2, b.heading2.as_ref(), b, by_id, opts)],
        5 => vec![heading(3, b.heading3.as_ref(), b, by_id, opts)],
        6 => vec![heading(4, b.heading4.as_ref(), b, by_id, opts)],
        7 => vec![heading(5, b.heading5.as_ref(), b, by_id, opts)],
        8 => vec![heading(6, b.heading6.as_ref(), b, by_id, opts)],
        9 => vec![heading(7, b.heading7.as_ref(), b, by_id, opts)],
        10 => vec![heading(8, b.heading8.as_ref(), b, by_id, opts)],
        11 => vec![heading(9, b.heading9.as_ref(), b, by_id, opts)],
        12 => vec![textish(BlockKind::Bullet, b.bullet.as_ref(), b, by_id, opts)],
        13 => vec![textish(BlockKind::Ordered { seq: None }, b.ordered.as_ref(), b, by_id, opts)],
        14 => vec![code_block(b)],
        15 => vec![textish(BlockKind::Quote, b.quote.as_ref(), b, by_id, opts)],
        17 => {
            let done = b.todo.as_ref().and_then(|t| t.style.as_ref()).map(|s| s.done).unwrap_or(false);
            let tc = b.todo.as_ref().map(|t| TextContainer { elements: t.elements.clone(), style: t.style.clone() });
            vec![textish(BlockKind::Todo { done }, tc.as_ref(), b, by_id, opts)]
        }
        19 => vec![callout(b, by_id, opts)],
        22 => vec![Block::new(BlockKind::Divider, RichText::plain(""))],
        23 => vec![file_block(b, opts)],
        24 => vec![grid(b, by_id, opts)],
        25 => {
            // Columns are folded into the parent grid.
            Vec::new()
        }
        27 => vec![image_block(b, opts)],
        30 => vec![sheet_block(b, opts)],
        31 => vec![table_block(b, by_id, opts)],
        32 => Vec::new(), // cells folded into table
        34 => {
            // quote_container: children become a Quote block's children.
            let mut kids = Vec::new();
            for id in &b.children {
                if let Some(c) = by_id.get(id.as_str()) {
                    kids.extend(map_block(c, by_id, opts));
                }
            }
            vec![Block::new(BlockKind::Quote, RichText::plain("")).with_children(kids)]
        }
        _ => vec![raw_block(b)],
    }
}

fn heading(level: u8, tc: Option<&TextContainer>, b: &ApiBlock, by_id: &HashMap<&str, &ApiBlock>, opts: &MapOptions) -> Block {
    textish(BlockKind::Heading { level, seq_auto: false }, tc, b, by_id, opts)
}

fn textish(
    kind: BlockKind,
    tc: Option<&TextContainer>,
    b: &ApiBlock,
    by_id: &HashMap<&str, &ApiBlock>,
    opts: &MapOptions,
) -> Block {
    let text = tc.map(|t| rich_from_elements(&t.elements)).unwrap_or_else(|| RichText::plain(""));
    let align = tc.and_then(|t| t.style.as_ref()).map(|s| align_of(s.align)).unwrap_or(Align::Left);
    let folded = tc.and_then(|t| t.style.as_ref()).map(|s| s.folded).unwrap_or(false);
    let mut block = Block::new(kind, text);
    block.align = align;
    block.folded = folded;
    let mut kids = Vec::new();
    for id in &b.children {
        if let Some(c) = by_id.get(id.as_str()) {
            kids.extend(map_block(c, by_id, opts));
        }
    }
    block.children = kids;
    block
}

fn code_block(b: &ApiBlock) -> Block {
    let code = b.code.as_ref();
    let text = code
        .map(|c| rich_from_elements(&c.elements))
        .unwrap_or_else(|| RichText::plain(""));
    // Strip marks: code blocks are plain in quiet.md.
    let plain = RichText::plain(&text.text);
    let lang = code
        .and_then(|c| c.style.as_ref())
        .map(|s| lang_name(s.language))
        .unwrap_or_default();
    let wrap = code.and_then(|c| c.style.as_ref()).map(|s| s.wrap).unwrap_or(false);
    Block::new(BlockKind::Code { lang, caption: None, wrap }, plain)
}

fn callout(b: &ApiBlock, by_id: &HashMap<&str, &ApiBlock>, opts: &MapOptions) -> Block {
    let c = b.callout.as_ref();
    let emoji = c.map(|c| c.emoji_id.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "💡".into());
    let bg = c.and_then(|c| callout_color(c.background_color)).map(ColorName);
    let border = c.and_then(|c| callout_color(c.border_color)).map(ColorName);
    let text_color = c.and_then(|c| callout_color(c.text_color)).map(ColorName);
    let mut kids = Vec::new();
    for id in &b.children {
        if let Some(ch) = by_id.get(id.as_str()) {
            kids.extend(map_block(ch, by_id, opts));
        }
    }
    Block::new(
        BlockKind::Callout { emoji, bg, border, text_color },
        RichText::plain(""),
    )
    .with_children(kids)
}

fn image_block(b: &ApiBlock, opts: &MapOptions) -> Block {
    let img = b.image.as_ref();
    let token = img.map(|i| i.token.as_str()).unwrap_or("");
    let src = opts
        .media
        .get(token)
        .cloned()
        .unwrap_or_else(|| format!("feishu-token:{token}"));
    let width = img.and_then(|i| (i.width > 0).then_some(i.width as f32));
    let height = img.and_then(|i| (i.height > 0).then_some(i.height as f32));
    let align = img.map(|i| align_of(i.align)).unwrap_or(Align::Left);
    let mut block = Block::new(
        BlockKind::Image {
            src_kind: "path".into(),
            src,
            width,
            height,
            caption: None,
            crop: None,
        },
        RichText::plain(""),
    );
    block.align = align;
    block
}

fn file_block(b: &ApiBlock, opts: &MapOptions) -> Block {
    let f = b.file.as_ref();
    let token = f.map(|f| f.token.as_str()).unwrap_or("");
    let name = f.map(|f| f.name.clone()).unwrap_or_default();
    let src = opts
        .media
        .get(token)
        .cloned()
        .unwrap_or_else(|| token.to_string());
    let src_kind = if opts.media.contains_key(token) { "path" } else { "token" }.to_string();
    Block::new(
        BlockKind::File(FileBlock { src_kind, src, name, view: FileView::Card }),
        RichText::plain(""),
    )
}

fn grid(b: &ApiBlock, by_id: &HashMap<&str, &ApiBlock>, opts: &MapOptions) -> Block {
    let mut cols = Vec::new();
    for id in &b.children {
        let Some(col) = by_id.get(id.as_str()) else { continue };
        if col.block_type != 25 {
            cols.extend(map_block(col, by_id, opts));
            continue;
        }
        let ratio = col.grid_column.as_ref().map(|g| g.width_ratio).filter(|r| *r > 0.).unwrap_or(1.);
        let mut kids = Vec::new();
        for cid in &col.children {
            if let Some(c) = by_id.get(cid.as_str()) {
                kids.extend(map_block(c, by_id, opts));
            }
        }
        cols.push(Block::new(BlockKind::Column { ratio }, RichText::plain("")).with_children(kids));
    }
    Block::new(BlockKind::Grid, RichText::plain("")).with_children(cols)
}

fn table_block(b: &ApiBlock, by_id: &HashMap<&str, &ApiBlock>, opts: &MapOptions) -> Block {
    let prop = b.table.as_ref().and_then(|t| t.property.as_ref());
    let rows_n = prop.map(|p| p.row_size.max(0) as usize).unwrap_or(0);
    let cols_n = prop.map(|p| p.column_size.max(0) as usize).unwrap_or(0);
    let cells = b.table.as_ref().map(|t| t.cells.as_slice()).unwrap_or(&[]);
    let merge_info = prop.map(|p| p.merge_info.as_slice()).unwrap_or(&[]);
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    if rows_n > 0 && cols_n > 0 {
        for r in 0..rows_n {
            let mut row = Vec::with_capacity(cols_n);
            for c in 0..cols_n {
                let idx = r * cols_n + c;
                let cell_id = cells.get(idx).map(|s| s.as_str()).unwrap_or("");
                let mut blocks = Vec::new();
                if let Some(cell) = by_id.get(cell_id) {
                    for cid in &cell.children {
                        if let Some(ch) = by_id.get(cid.as_str()) {
                            blocks.extend(map_block(ch, by_id, opts));
                        }
                    }
                }
                if blocks.is_empty() {
                    blocks.push(Block::text(""));
                }
                row.push(TableCell {
                    blocks,
                    bg: None,
                    valign: None,
                    colspan: 1,
                    rowspan: 1,
                });
            }
            rows.push(row);
        }
        apply_table_merge_info(&mut rows, merge_info);
    }
    let col_widths = prop
        .map(|p| p.column_width.iter().map(|w| *w as f32).collect())
        .unwrap_or_default();
    let header_rows = if prop.map(|p| p.header_row).unwrap_or(false) { 1 } else { 0 };
    let mut table = Table { col_widths, header_rows, header_col: false, rows };
    table.repair();
    Block::new(BlockKind::Table(table), RichText::plain(""))
}

/// Map Feishu `table.property.merge_info` (row-major, parallel to `cells`) onto
/// quiet.md [`TableCell`] spans. Anchors keep content; covered slots become
/// [`TableCell::covered`]. API quirk: covered (or buggy) entries may report
/// `row_span`/`col_span` as 0 — treat those as 1 when the slot is still free.
fn apply_table_merge_info(rows: &mut [Vec<TableCell>], merge_info: &[crate::feishu::types::TableMergeInfo]) {
    let rows_n = rows.len();
    if rows_n == 0 {
        return;
    }
    let cols_n = rows[0].len();
    if cols_n == 0 {
        return;
    }
    let mut owned = vec![vec![false; cols_n]; rows_n];
    for r in 0..rows_n {
        for c in 0..cols_n {
            if owned[r][c] {
                rows[r][c] = TableCell::covered();
                continue;
            }
            let idx = r * cols_n + c;
            let (mut rs, mut cs) = match merge_info.get(idx) {
                Some(m) => (m.row_span.max(0) as usize, m.col_span.max(0) as usize),
                None => (1, 1),
            };
            // Feishu occasionally returns {0,0} for covered/buggy slots.
            if rs == 0 {
                rs = 1;
            }
            if cs == 0 {
                cs = 1;
            }
            rs = rs.min(rows_n - r);
            cs = cs.min(cols_n - c);
            rows[r][c].rowspan = rs as u32;
            rows[r][c].colspan = cs as u32;
            for rr in r..r + rs {
                for cc in c..c + cs {
                    owned[rr][cc] = true;
                    if rr != r || cc != c {
                        rows[rr][cc] = TableCell::covered();
                    }
                }
            }
        }
    }
}

fn sheet_block(b: &ApiBlock, opts: &MapOptions) -> Block {
    if let Some(rows) = opts.sheets.get(&b.block_id) {
        if !rows.is_empty() {
            return sheet_rows_to_table(rows);
        }
    }
    // Missing or empty fetch → readable placeholder, never raw JSON.
    let tag = "feishu-sheet".to_string();
    let label = unsupported_label("sheet");
    let token = b.sheet.as_ref().map(|s| s.token.as_str()).unwrap_or("");
    let xml = if token.is_empty() {
        format!(
            "<{tag} type=\"30\" kind=\"sheet\" label=\"{}\">{label}（无内容）</{tag}>",
            escape_xml(label)
        )
    } else {
        format!(
            "<{tag} type=\"30\" kind=\"sheet\" token=\"{token}\" label=\"{lab}\">{label}（未能加载内容）</{tag}>",
            token = escape_xml(token),
            lab = escape_xml(label),
        )
    };
    Block::new(BlockKind::Raw { tag, xml }, RichText::plain(""))
}

fn sheet_rows_to_table(rows: &[Vec<String>]) -> Block {
    let cols_n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut table_rows: Vec<Vec<TableCell>> = Vec::with_capacity(rows.len());
    for r in rows {
        let mut row = Vec::with_capacity(cols_n);
        for c in 0..cols_n {
            let text = r.get(c).cloned().unwrap_or_default();
            row.push(TableCell {
                blocks: vec![Block::text(&text)],
                bg: None,
                valign: None,
                colspan: 1,
                rowspan: 1,
            });
        }
        table_rows.push(row);
    }
    Block::new(
        BlockKind::Table(Table {
            col_widths: Vec::new(),
            header_rows: 1,
            header_col: false,
            rows: table_rows,
        }),
        RichText::plain(""),
    )
}

/// Friendly slug for unsupported Feishu block types (used in Raw tag / kind attr).
fn unsupported_kind(block_type: i32) -> &'static str {
    match block_type {
        18 => "bitable",
        20 => "chat_card",
        26 => "iframe",
        28 => "isv",
        29 => "mindnote",
        30 => "sheet",
        33 => "view",
        35 => "task",
        43 => "board",
        44 => "agenda",
        48 => "link_preview",
        _ => "unsupported",
    }
}

/// Chinese UI label for an unsupported Feishu kind slug (`board`, `sheet`, …).
pub fn unsupported_label(kind: &str) -> &'static str {
    match kind {
        "sheet" => "飞书电子表格",
        "board" => "飞书画板",
        "bitable" => "飞书多维表格",
        "mindnote" => "飞书思维导图",
        "chat_card" => "飞书会话卡片",
        "iframe" => "飞书内嵌网页",
        "view" => "飞书视图",
        "task" => "飞书任务",
        "agenda" => "飞书议程",
        "link_preview" => "飞书链接预览",
        "isv" => "飞书应用块",
        _ => "飞书未支持块",
    }
}

/// Label for a Raw tag such as `feishu-board` (used by the editor card UI).
pub fn label_for_raw_tag(tag: &str) -> Option<&'static str> {
    let kind = tag.strip_prefix("feishu-").unwrap_or(tag);
    if tag.starts_with("feishu-")
        || matches!(
            kind,
            "sheet"
                | "board"
                | "bitable"
                | "mindnote"
                | "chat_card"
                | "iframe"
                | "view"
                | "task"
                | "agenda"
                | "link_preview"
                | "isv"
                | "unsupported"
        )
    {
        Some(unsupported_label(kind))
    } else {
        None
    }
}

/// Human card title + optional open URL extracted from Raw XML.
///
/// Prefers the `label` attribute (aligned with import), then element text,
/// then the Feishu tag → Chinese label table.
pub fn raw_card_info(tag: &str, xml: &str) -> (String, Option<String>) {
    let (text, link) = crate::docxml::raw_summary(xml);
    let label = crate::docxml::raw_attr(xml, "label")
        .filter(|s| !s.trim().is_empty())
        .or_else(|| (!text.is_empty()).then_some(text))
        .or_else(|| label_for_raw_tag(tag).map(str::to_string))
        .unwrap_or_else(|| tag.to_string());
    let open = link.filter(|u| {
        let u = u.trim();
        u.starts_with("http://") || u.starts_with("https://")
    });
    (label, open)
}

fn raw_block(b: &ApiBlock) -> Block {
    // Never dump API JSON into document XML — always a well-formed element.
    let kind = unsupported_kind(b.block_type);
    let tag = if kind == "unsupported" {
        "feishu-unsupported".to_string()
    } else {
        format!("feishu-{kind}")
    };
    let label = unsupported_label(kind);
    let mut attrs = format!(
        "type=\"{}\" kind=\"{kind}\" label=\"{}\"",
        b.block_type,
        escape_xml(label)
    );
    if let Some(sheet) = &b.sheet {
        if !sheet.token.is_empty() {
            attrs.push_str(&format!(" token=\"{}\"", escape_xml(&sheet.token)));
        }
    }
    if let Some(board) = &b.board {
        if !board.token.is_empty() {
            attrs.push_str(&format!(" token=\"{}\"", escape_xml(&board.token)));
        }
    }
    let xml = format!("<{tag} {attrs}>{label}</{tag}>");
    Block::new(BlockKind::Raw { tag, xml }, RichText::plain(""))
}

fn rich_from_elements(elements: &[TextElement]) -> RichText {
    let mut rt = RichText::plain("");
    for el in elements {
        if let Some(run) = &el.text_run {
            let marks = marks_from(run.text_element_style.as_ref());
            rt.push(&run.content, marks);
        } else if let Some(eq) = &el.equation {
            let xml = format!("<latex>{}</latex>", escape_xml(&eq.content));
            let mut marks = Marks::default();
            marks.inline = Some(Inline {
                kind: InlineKind::Equation(eq.content.clone()),
                xml,
            });
            rt.push("\u{fffc}", marks);
        } else if let Some(doc) = &el.mention_doc {
            push_mention_doc(&mut rt, doc);
        } else if let Some(user) = &el.mention_user {
            push_mention_user(&mut rt, user);
        } else if let Some(rem) = &el.reminder {
            push_reminder(&mut rt, rem);
        }
        // Soft-fail: unknown / empty TextElement variants contribute nothing.
    }
    rt
}

/// `@文档` → `InlineDocLink` (token stored as `id` for future local resolve).
/// Falls back to a titled hyperlink when the token is missing but a URL exists.
fn push_mention_doc(rt: &mut RichText, doc: &MentionDoc) {
    if doc.token.is_empty() && doc.title.is_empty() && doc.url.is_empty() {
        return;
    }
    let title = if !doc.title.is_empty() {
        doc.title.clone()
    } else if !doc.token.is_empty() {
        doc.token.clone()
    } else {
        "文档".into()
    };
    if !doc.token.is_empty() {
        let inline = crate::docxml::to_inline(InlineKind::DocLink(InlineDocLink {
            id: doc.token.clone(),
            title,
        }));
        let mut marks = Marks::default();
        marks.inline = Some(inline);
        rt.push("\u{fffc}", marks);
        return;
    }
    let mut marks = Marks::default();
    if !doc.url.is_empty() {
        marks.link = Some(doc.url.clone());
    }
    rt.push(&title, marks);
}

/// `@user` → readable `@{name}` when the payload carries a name field, else `@用户`.
fn push_mention_user(rt: &mut RichText, user: &Value) {
    let name = ["name", "user_name", "display_name", "nickname"]
        .iter()
        .find_map(|k| user.get(*k))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let label = match name {
        Some(n) => format!("@{n}"),
        None => "@用户".into(),
    };
    rt.push(&label, Marks::default());
}

/// Date reminder → `InlineReminder` when `expire_time` is present; otherwise soft-skip.
fn push_reminder(rt: &mut RichText, rem: &Value) {
    let Some(expire) = json_i64(rem.get("expire_time")) else {
        return;
    };
    let notify = json_i64(rem.get("notify_time")).unwrap_or(expire);
    let should_notify = rem
        .get("is_notify")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let whole_day = rem
        .get("is_whole_day")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let inline = crate::docxml::to_inline(InlineKind::Reminder(InlineReminder {
        expire,
        notify,
        should_notify,
        whole_day,
    }));
    let mut marks = Marks::default();
    marks.inline = Some(inline);
    rt.push("\u{fffc}", marks);
}

fn json_i64(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(n) = v.as_u64() {
        return n.try_into().ok();
    }
    if let Some(n) = v.as_f64() {
        return Some(n as i64);
    }
    v.as_str().and_then(|s| s.trim().parse().ok())
}

fn marks_from(style: Option<&TextElementStyle>) -> Marks {
    let Some(s) = style else {
        return Marks::default();
    };
    Marks {
        bold: s.bold,
        italic: s.italic,
        underline: s.underline,
        strike: s.strikethrough,
        code: s.inline_code,
        link: s.link.as_ref().map(|l| l.url.clone()).filter(|u| !u.is_empty()),
        text_color: text_color(s.text_color),
        bg_color: bg_color(s.background_color),
        inline: None,
    }
}

fn align_of(n: i32) -> Align {
    match n {
        2 => Align::Center,
        3 => Align::Right,
        _ => Align::Left,
    }
}

/// Feishu text / highlight color enums → quiet.md palette names.
fn text_color(n: i32) -> Option<ColorName> {
    let name = match n {
        1 => "red",
        2 => "orange",
        3 => "yellow",
        4 => "green",
        5 => "blue",
        6 => "purple",
        7 => "gray",
        _ => return None,
    };
    Some(ColorName(name.into()))
}

fn bg_color(n: i32) -> Option<ColorName> {
    text_color(n)
}

fn callout_color(n: i32) -> Option<String> {
    match n {
        1 => Some("red".into()),
        2 => Some("orange".into()),
        3 => Some("yellow".into()),
        4 => Some("green".into()),
        5 => Some("blue".into()),
        6 => Some("purple".into()),
        7 => Some("gray".into()),
        _ => None,
    }
}

fn lang_name(n: i32) -> String {
    // Subset of Feishu `CodeLanguage`.
    match n {
        1 => "plaintext",
        2 => "abap",
        3 => "ada",
        4 => "assembly",
        5 => "haskell",
        22 => "c",
        23 => "cpp",
        24 => "csharp",
        25 => "css",
        27 => "dart",
        28 => "dockerfile",
        30 => "go",
        32 => "html",
        33 => "java",
        34 => "javascript",
        36 => "json",
        37 => "julia",
        39 => "kotlin",
        40 => "latex",
        42 => "lua",
        43 => "markdown",
        46 => "objective-c",
        48 => "perl",
        49 => "php",
        50 => "powershell",
        51 => "prolog",
        52 => "protobuf",
        53 => "python",
        54 => "r",
        56 => "ruby",
        57 => "rust",
        58 => "sas",
        59 => "scala",
        60 => "scheme",
        62 => "shell",
        63 => "sql",
        64 => "swift",
        65 => "thrift",
        66 => "typescript",
        69 => "xml",
        70 => "yaml",
        _ => "",
    }
    .into()
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feishu::types::{MentionDoc, TextElement, TextRun};
    use crate::model::{InlineDocLink, InlineKind, InlineReminder};
    use serde_json::json;

    fn text_block(id: &str, parent: &str, content: &str, ty: i32) -> ApiBlock {
        let el = TextElement {
            text_run: Some(TextRun {
                content: content.into(),
                text_element_style: None,
            }),
            mention_user: None,
            mention_doc: None,
            equation: None,
            reminder: None,
        };
        let tc = TextContainer { elements: vec![el], style: None };
        let mut b = ApiBlock {
            block_id: id.into(),
            parent_id: parent.into(),
            block_type: ty,
            children: vec![],
            page: None,
            text: None,
            heading1: None,
            heading2: None,
            heading3: None,
            heading4: None,
            heading5: None,
            heading6: None,
            heading7: None,
            heading8: None,
            heading9: None,
            bullet: None,
            ordered: None,
            code: None,
            quote: None,
            todo: None,
            callout: None,
            divider: None,
            image: None,
            file: None,
            table: None,
            table_cell: None,
            grid: None,
            grid_column: None,
            quote_container: None,
            sheet: None,
            board: None,
            raw_json: None,
        };
        match ty {
            1 => b.page = Some(tc),
            2 => b.text = Some(tc),
            3 => b.heading1 = Some(tc),
            12 => b.bullet = Some(tc),
            _ => b.text = Some(tc),
        }
        b
    }

    #[test]
    fn maps_cover_from_meta_media() {
        let page = {
            let mut p = text_block("p", "", "T", 1);
            p.children = vec![];
            p
        };
        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 1,
            title: "T".into(),
            cover: Some(crate::feishu::types::DocumentCover {
                token: "COVTOK".into(),
                offset_ratio_x: 0.1,
                offset_ratio_y: -0.25,
            }),
            ..Default::default()
        };
        let mut opts = MapOptions::default();
        opts.media.insert("COVTOK".into(), "/tmp/assets/COVTOK".into());
        let doc = document_from_blocks(&meta, &[page], &opts);
        assert_eq!(
            doc.cover,
            Some(Cover { src: "/tmp/assets/COVTOK".into(), x: 0.1, y: -0.25 })
        );
        assert!(doc.icon.is_none());
    }

    #[test]
    fn cover_without_downloaded_media_is_none() {
        let page = {
            let mut p = text_block("p", "", "T", 1);
            p.children = vec![];
            p
        };
        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 1,
            title: "T".into(),
            cover: Some(crate::feishu::types::DocumentCover {
                token: "MISSING".into(),
                offset_ratio_x: 0.,
                offset_ratio_y: 0.,
            }),
            ..Default::default()
        };
        let doc = document_from_blocks(&meta, &[page], &MapOptions::default());
        assert!(doc.cover.is_none());
        assert!(doc.icon.is_none());
    }

    #[test]
    fn maps_heading_and_bullet() {
        let page = text_block("p", "", "Title", 1);
        let mut page = page;
        page.children = vec!["h".into(), "b".into()];
        let h = text_block("h", "p", "Hello", 3);
        let bullet = text_block("b", "p", "item", 12);
        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 1,
            title: "Title".into(),
            ..Default::default()
        };
        let doc = document_from_blocks(&meta, &[page, h, bullet], &MapOptions::default());
        assert_eq!(doc.title.text, "Title");
        assert_eq!(doc.blocks.len(), 2);
        assert!(matches!(doc.blocks[0].kind, BlockKind::Heading { level: 1, .. }));
        assert_eq!(doc.blocks[0].text.text, "Hello");
        assert!(matches!(doc.blocks[1].kind, BlockKind::Bullet));
        assert_eq!(doc.blocks[1].text.text, "item");
    }

    #[test]
    fn unknown_becomes_raw() {
        let page = {
            let mut p = text_block("p", "", "T", 1);
            p.children = vec!["x".into()];
            p
        };
        let mut unknown = text_block("x", "p", "", 99);
        unknown.block_type = 99;
        unknown.raw_json = Some(r#"{"block_id":"x","block_type":99}"#.into());
        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 0,
            title: "T".into(),
            ..Default::default()
        };
        let doc = document_from_blocks(&meta, &[page, unknown], &MapOptions::default());
        let BlockKind::Raw { tag, xml } = &doc.blocks[0].kind else {
            panic!("expected Raw");
        };
        assert_eq!(tag, "feishu-unsupported");
        assert!(xml.contains("feishu-unsupported"), "{xml}");
        assert!(!xml.contains('{'), "must not dump JSON: {xml}");
        assert!(xml.contains("飞书未支持块"), "{xml}");
    }

    #[test]
    fn sheet_becomes_table() {
        let page = {
            let mut p = text_block("p", "", "T", 1);
            p.children = vec!["s".into()];
            p
        };
        let mut sheet = text_block("s", "p", "", 30);
        sheet.block_type = 30;
        sheet.sheet = Some(crate::feishu::types::SheetApiBlock {
            token: "SSSS_sheet1".into(),
        });
        sheet.raw_json = Some(r#"{"block_id":"s","block_type":30,"sheet":{"token":"SSSS_sheet1"}}"#.into());
        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 0,
            title: "T".into(),
            ..Default::default()
        };
        let mut opts = MapOptions::default();
        opts.sheets.insert(
            "s".into(),
            vec![
                vec!["指标".into(), "目标".into(), "实际".into()],
                vec!["转化率".into(), "10%".into(), "12%".into()],
            ],
        );
        let doc = document_from_blocks(&meta, &[page, sheet], &opts);
        let BlockKind::Table(t) = &doc.blocks[0].kind else {
            panic!("expected Table, got {:?}", doc.blocks[0].kind);
        };
        assert_eq!(t.header_rows, 1);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].len(), 3);
        assert_eq!(t.rows[0][0].blocks[0].text.text, "指标");
        assert_eq!(t.rows[1][2].blocks[0].text.text, "12%");
    }

    /// Build a Feishu table (block_type 31) with cell text blocks and optional merge_info.
    fn feishu_table_fixture(
        cell_texts: &[&str],
        rows_n: i32,
        cols_n: i32,
        merge_info: Vec<crate::feishu::types::TableMergeInfo>,
    ) -> (DocumentMeta, Vec<ApiBlock>) {
        let mut page = text_block("p", "", "T", 1);
        page.children = vec!["tbl".into()];

        let mut cell_ids = Vec::new();
        let mut blocks = vec![page];
        for (i, txt) in cell_texts.iter().enumerate() {
            let cid = format!("c{i}");
            let tid = format!("t{i}");
            let mut cell = text_block(&cid, "tbl", "", 32);
            cell.block_type = 32;
            cell.children = vec![tid.clone()];
            let text = text_block(&tid, &cid, txt, 2);
            cell_ids.push(cid);
            blocks.push(cell);
            blocks.push(text);
        }

        let mut table = text_block("tbl", "p", "", 31);
        table.block_type = 31;
        table.children = cell_ids.clone();
        table.table = Some(crate::feishu::types::TableBlock {
            property: Some(crate::feishu::types::TableProperty {
                row_size: rows_n,
                column_size: cols_n,
                column_width: vec![100; cols_n as usize],
                header_row: false,
                merge_info,
            }),
            cells: cell_ids,
        });
        blocks.insert(1, table);

        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 0,
            title: "T".into(),
            ..Default::default()
        };
        (meta, blocks)
    }

    #[test]
    fn table_merge_info_maps_colspan_rowspan() {
        // 2x2 table: top-left spans both rows and cols; others covered (0,0 quirk).
        let merge_info = vec![
            crate::feishu::types::TableMergeInfo { row_span: 2, col_span: 2 },
            crate::feishu::types::TableMergeInfo { row_span: 0, col_span: 0 },
            crate::feishu::types::TableMergeInfo { row_span: 0, col_span: 0 },
            crate::feishu::types::TableMergeInfo { row_span: 0, col_span: 0 },
        ];
        let (meta, blocks) = feishu_table_fixture(&["A", "b", "c", "d"], 2, 2, merge_info);
        let doc = document_from_blocks(&meta, &blocks, &MapOptions::default());
        let BlockKind::Table(t) = &doc.blocks[0].kind else {
            panic!("expected Table, got {:?}", doc.blocks[0].kind);
        };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].len(), 2);
        assert_eq!((t.rows[0][0].rowspan, t.rows[0][0].colspan), (2, 2));
        assert_eq!(t.rows[0][0].blocks[0].text.text, "A");
        assert!(t.rows[0][1].is_covered());
        assert!(t.rows[1][0].is_covered());
        assert!(t.rows[1][1].is_covered());
    }

    #[test]
    fn table_partial_row_merge_and_unmerged() {
        // 2x3: first row merges col0-1; rest are 1x1. Covered slot may still say 1,1.
        let merge_info = vec![
            crate::feishu::types::TableMergeInfo { row_span: 1, col_span: 2 },
            crate::feishu::types::TableMergeInfo { row_span: 1, col_span: 1 },
            crate::feishu::types::TableMergeInfo { row_span: 1, col_span: 1 },
            crate::feishu::types::TableMergeInfo { row_span: 1, col_span: 1 },
            crate::feishu::types::TableMergeInfo { row_span: 1, col_span: 1 },
            crate::feishu::types::TableMergeInfo { row_span: 1, col_span: 1 },
        ];
        let (meta, blocks) =
            feishu_table_fixture(&["AB", "x", "C", "D", "E", "F"], 2, 3, merge_info);
        let doc = document_from_blocks(&meta, &blocks, &MapOptions::default());
        let BlockKind::Table(t) = &doc.blocks[0].kind else {
            panic!("expected Table");
        };
        assert_eq!((t.rows[0][0].rowspan, t.rows[0][0].colspan), (1, 2));
        assert_eq!(t.rows[0][0].blocks[0].text.text, "AB");
        assert!(t.rows[0][1].is_covered());
        assert_eq!((t.rows[0][2].rowspan, t.rows[0][2].colspan), (1, 1));
        assert_eq!(t.rows[0][2].blocks[0].text.text, "C");
        assert_eq!(t.rows[1][0].blocks[0].text.text, "D");
        assert!(!t.rows[1][0].is_covered());
    }

    #[test]
    fn table_without_merge_info_stays_1x1() {
        let (meta, blocks) = feishu_table_fixture(&["a", "b", "c", "d"], 2, 2, vec![]);
        let doc = document_from_blocks(&meta, &blocks, &MapOptions::default());
        let BlockKind::Table(t) = &doc.blocks[0].kind else {
            panic!("expected Table");
        };
        for row in &t.rows {
            for cell in row {
                assert!(!cell.is_covered());
                assert_eq!((cell.rowspan, cell.colspan), (1, 1));
            }
        }
    }

    #[test]
    fn sheet_without_data_is_placeholder_not_json() {
        let page = {
            let mut p = text_block("p", "", "T", 1);
            p.children = vec!["s".into()];
            p
        };
        let mut sheet = text_block("s", "p", "", 30);
        sheet.block_type = 30;
        sheet.sheet = Some(crate::feishu::types::SheetApiBlock {
            token: "SSSS_sheet1".into(),
        });
        sheet.raw_json = Some(r#"{"block_type":30}"#.into());
        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 0,
            title: "T".into(),
            ..Default::default()
        };
        let doc = document_from_blocks(&meta, &[page, sheet], &MapOptions::default());
        let BlockKind::Raw { tag, xml } = &doc.blocks[0].kind else {
            panic!("expected Raw placeholder");
        };
        assert_eq!(tag, "feishu-sheet");
        assert!(!xml.contains('{'), "{xml}");
        assert!(xml.contains("飞书电子表格"), "{xml}");
    }

    #[test]
    fn board_becomes_labeled_raw() {
        let page = {
            let mut p = text_block("p", "", "T", 1);
            p.children = vec!["b".into()];
            p
        };
        let mut board = text_block("b", "p", "", 43);
        board.block_type = 43;
        board.board = Some(crate::feishu::types::BoardApiBlock {
            token: "BOARDTOK".into(),
        });
        board.raw_json = Some(r#"{"block_type":43,"board":{"token":"BOARDTOK"}}"#.into());
        let meta = DocumentMeta {
            document_id: "d".into(),
            revision_id: 0,
            title: "T".into(),
            ..Default::default()
        };
        let doc = document_from_blocks(&meta, &[page, board], &MapOptions::default());
        let BlockKind::Raw { tag, xml } = &doc.blocks[0].kind else {
            panic!("expected Raw");
        };
        assert_eq!(tag, "feishu-board");
        assert!(!xml.contains('{'), "{xml}");
        assert!(xml.contains("飞书画板"), "{xml}");
        assert!(xml.contains("BOARDTOK"), "{xml}");
        assert!(xml.contains("label=\"飞书画板\""), "{xml}");
        let (card, open) = raw_card_info(tag, xml);
        assert_eq!(card, "飞书画板");
        assert!(open.is_none());
    }

    #[test]
    fn raw_card_prefers_xml_text_and_http_link() {
        let xml = r#"<feishu-board type="43" kind="board" label="飞书画板" url="https://my.feishu.cn/board/TOK">飞书画板</feishu-board>"#;
        let (label, open) = raw_card_info("feishu-board", xml);
        assert_eq!(label, "飞书画板");
        assert_eq!(open.as_deref(), Some("https://my.feishu.cn/board/TOK"));
        assert_eq!(label_for_raw_tag("feishu-bitable"), Some("飞书多维表格"));
    }

    fn empty_el() -> TextElement {
        TextElement {
            text_run: None,
            mention_user: None,
            mention_doc: None,
            equation: None,
            reminder: None,
        }
    }

    #[test]
    fn mention_doc_becomes_inline_doc_link() {
        let mut el = empty_el();
        el.mention_doc = Some(MentionDoc {
            token: "TOK123".into(),
            title: "设计稿".into(),
            url: "https://example.feishu.cn/docx/TOK123".into(),
            obj_type: 22,
        });
        let rt = rich_from_elements(&[el]);
        assert_eq!(rt.text, "\u{fffc}");
        let inline = rt.runs[0].marks.inline.as_ref().expect("inline");
        assert_eq!(
            inline.doc_link(),
            Some(&InlineDocLink {
                id: "TOK123".into(),
                title: "设计稿".into(),
            })
        );
        assert!(inline.xml.contains("cite"), "{}", inline.xml);
        assert!(inline.xml.contains("TOK123"), "{}", inline.xml);
        assert!(inline.xml.contains("设计稿"), "{}", inline.xml);
    }

    #[test]
    fn mention_doc_without_title_uses_token() {
        let mut el = empty_el();
        el.mention_doc = Some(MentionDoc {
            token: "ONLYTOK".into(),
            ..Default::default()
        });
        let rt = rich_from_elements(&[el]);
        let link = rt.runs[0].marks.inline.as_ref().unwrap().doc_link().unwrap();
        assert_eq!(link.id, "ONLYTOK");
        assert_eq!(link.title, "ONLYTOK");
    }

    #[test]
    fn mention_doc_url_only_becomes_titled_link() {
        let mut el = empty_el();
        el.mention_doc = Some(MentionDoc {
            title: "外链文档".into(),
            url: "https://example.feishu.cn/docx/x".into(),
            ..Default::default()
        });
        let rt = rich_from_elements(&[el]);
        assert_eq!(rt.text, "外链文档");
        assert_eq!(
            rt.runs[0].marks.link.as_deref(),
            Some("https://example.feishu.cn/docx/x")
        );
        assert!(rt.runs[0].marks.inline.is_none());
    }

    #[test]
    fn mention_user_uses_name_or_fallback() {
        let mut named = empty_el();
        named.mention_user = Some(json!({"user_id": "ou_1", "name": "张三"}));
        let mut bare = empty_el();
        bare.mention_user = Some(json!({"user_id": "ou_2"}));
        let rt = rich_from_elements(&[named, bare]);
        assert_eq!(rt.text, "@张三@用户");
    }

    #[test]
    fn reminder_becomes_inline_reminder() {
        let mut el = empty_el();
        el.reminder = Some(json!({
            "create_user_id": "ou_x",
            "is_notify": true,
            "is_whole_day": true,
            "expire_time": 1_700_000_000_000_i64,
            "notify_time": 1_699_913_600_000_i64
        }));
        let rt = rich_from_elements(&[el]);
        assert_eq!(rt.text, "\u{fffc}");
        let rem = rt.runs[0]
            .marks
            .inline
            .as_ref()
            .unwrap()
            .reminder()
            .copied()
            .unwrap();
        assert_eq!(
            rem,
            InlineReminder {
                expire: 1_700_000_000_000,
                notify: 1_699_913_600_000,
                should_notify: true,
                whole_day: true,
            }
        );
        assert!(matches!(
            rt.runs[0].marks.inline.as_ref().unwrap().kind,
            InlineKind::Reminder(_)
        ));
    }

    #[test]
    fn reminder_string_expire_time_parses() {
        let mut el = empty_el();
        el.reminder = Some(json!({
            "expire_time": "1700000000000",
            "notify_time": "1700000000000",
            "is_notify": false,
            "is_whole_day": false
        }));
        let rt = rich_from_elements(&[el]);
        let rem = rt.runs[0].marks.inline.as_ref().unwrap().reminder().unwrap();
        assert_eq!(rem.expire, 1_700_000_000_000);
        assert!(!rem.should_notify);
        assert!(!rem.whole_day);
    }

    #[test]
    fn reminder_without_expire_soft_fails() {
        let mut el = empty_el();
        el.reminder = Some(json!({"is_notify": true}));
        let rt = rich_from_elements(&[el]);
        assert!(rt.text.is_empty());
        assert!(rt.runs.is_empty());
    }

    #[test]
    fn unknown_text_element_soft_fails() {
        let rt = rich_from_elements(&[empty_el()]);
        assert!(rt.text.is_empty());
    }
}
