//! Map Feishu Open API blocks into quiet.md `Document` / `Block`.

use crate::model::{
    Align, Block, BlockKind, ColorName, Document, FileBlock, FileView, Inline, InlineKind,
    Marks, RichText, Table, TableCell,
};
use crate::feishu::types::{ApiBlock, DocumentMeta, TextContainer, TextElement, TextElementStyle};
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
    Document { title, icon: None, cover: None, blocks: out }
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
    }
    let col_widths = prop
        .map(|p| p.column_width.iter().map(|w| *w as f32).collect())
        .unwrap_or_default();
    let header_rows = if prop.map(|p| p.header_row).unwrap_or(false) { 1 } else { 0 };
    Block::new(
        BlockKind::Table(Table { col_widths, header_rows, header_col: false, rows }),
        RichText::plain(""),
    )
}

fn sheet_block(b: &ApiBlock, opts: &MapOptions) -> Block {
    if let Some(rows) = opts.sheets.get(&b.block_id) {
        if !rows.is_empty() {
            return sheet_rows_to_table(rows);
        }
    }
    // Missing or empty fetch → readable placeholder, never raw JSON.
    let tag = "feishu-sheet".to_string();
    let token = b.sheet.as_ref().map(|s| s.token.as_str()).unwrap_or("");
    let xml = if token.is_empty() {
        format!("<{tag} type=\"30\">飞书电子表格（无内容）</{tag}>")
    } else {
        format!(
            "<{tag} type=\"30\" token=\"{}\">飞书电子表格（未能加载内容）</{tag}>",
            escape_xml(token)
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

fn unsupported_label(kind: &str) -> &'static str {
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

fn raw_block(b: &ApiBlock) -> Block {
    // Never dump API JSON into document XML — always a well-formed element.
    let kind = unsupported_kind(b.block_type);
    let tag = if kind == "unsupported" {
        "feishu-unsupported".to_string()
    } else {
        format!("feishu-{kind}")
    };
    let mut attrs = format!("type=\"{}\" kind=\"{}\"", b.block_type, kind);
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
    let label = unsupported_label(kind);
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
            let title = if doc.title.is_empty() { &doc.token } else { &doc.title };
            let mut marks = Marks::default();
            marks.link = Some(format!("feishu-doc:{}", doc.token));
            rt.push(title, marks);
        } else if el.mention_user.is_some() {
            rt.push("@user", Marks::default());
        }
    }
    rt
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
    use crate::feishu::types::{TextElement, TextRun};

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
        };
        let doc = document_from_blocks(&meta, &[page, board], &MapOptions::default());
        let BlockKind::Raw { tag, xml } = &doc.blocks[0].kind else {
            panic!("expected Raw");
        };
        assert_eq!(tag, "feishu-board");
        assert!(!xml.contains('{'), "{xml}");
        assert!(xml.contains("飞书画板"), "{xml}");
        assert!(xml.contains("BOARDTOK"), "{xml}");
    }
}
