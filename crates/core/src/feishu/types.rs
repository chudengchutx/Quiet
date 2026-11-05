//! Feishu Open API document / block JSON shapes we care about for import.

use serde::Deserialize;
use serde_json::Value;

/// `GET /docx/v1/documents/:document_id` → `data.document`.
#[derive(Clone, Debug, Deserialize)]
pub struct DocumentMeta {
    #[serde(default)]
    pub document_id: String,
    #[serde(default)]
    pub revision_id: i64,
    #[serde(default)]
    pub title: String,
}

/// One item from `GET .../blocks` (`data.items[]`).
#[derive(Clone, Debug, Deserialize)]
pub struct ApiBlock {
    pub block_id: String,
    #[serde(default)]
    pub parent_id: String,
    pub block_type: i32,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub page: Option<TextContainer>,
    #[serde(default)]
    pub text: Option<TextContainer>,
    #[serde(default)]
    pub heading1: Option<TextContainer>,
    #[serde(default)]
    pub heading2: Option<TextContainer>,
    #[serde(default)]
    pub heading3: Option<TextContainer>,
    #[serde(default)]
    pub heading4: Option<TextContainer>,
    #[serde(default)]
    pub heading5: Option<TextContainer>,
    #[serde(default)]
    pub heading6: Option<TextContainer>,
    #[serde(default)]
    pub heading7: Option<TextContainer>,
    #[serde(default)]
    pub heading8: Option<TextContainer>,
    #[serde(default)]
    pub heading9: Option<TextContainer>,
    #[serde(default)]
    pub bullet: Option<TextContainer>,
    #[serde(default)]
    pub ordered: Option<TextContainer>,
    #[serde(default)]
    pub code: Option<CodeBlock>,
    #[serde(default)]
    pub quote: Option<TextContainer>,
    #[serde(default)]
    pub todo: Option<TodoBlock>,
    #[serde(default)]
    pub callout: Option<CalloutBlock>,
    #[serde(default)]
    pub divider: Option<Value>,
    #[serde(default)]
    pub image: Option<ImageBlock>,
    #[serde(default)]
    pub file: Option<FileApiBlock>,
    #[serde(default)]
    pub table: Option<TableBlock>,
    #[serde(default)]
    pub table_cell: Option<Value>,
    #[serde(default)]
    pub grid: Option<GridBlock>,
    #[serde(default)]
    pub grid_column: Option<GridColumnBlock>,
    #[serde(default)]
    pub quote_container: Option<Value>,
    /// Full JSON of this block (set by the importer for Round-trip Raw).
    #[serde(skip)]
    pub raw_json: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TextContainer {
    #[serde(default)]
    pub elements: Vec<TextElement>,
    #[serde(default)]
    pub style: Option<TextStyle>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TextStyle {
    #[serde(default)]
    pub align: i32,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub folded: bool,
    #[serde(default)]
    pub language: i32,
    #[serde(default)]
    pub wrap: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextElement {
    #[serde(default)]
    pub text_run: Option<TextRun>,
    #[serde(default)]
    pub mention_user: Option<Value>,
    #[serde(default)]
    pub mention_doc: Option<MentionDoc>,
    #[serde(default)]
    pub equation: Option<Equation>,
    #[serde(default)]
    pub reminder: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextRun {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub text_element_style: Option<TextElementStyle>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TextElementStyle {
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub strikethrough: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub inline_code: bool,
    #[serde(default)]
    pub link: Option<Link>,
    #[serde(default)]
    pub text_color: i32,
    #[serde(default)]
    pub background_color: i32,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct Link {
    #[serde(default)]
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Equation {
    #[serde(default)]
    pub content: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MentionDoc {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct CodeBlock {
    #[serde(default)]
    pub elements: Vec<TextElement>,
    #[serde(default)]
    pub style: Option<TextStyle>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TodoBlock {
    #[serde(default)]
    pub elements: Vec<TextElement>,
    #[serde(default)]
    pub style: Option<TextStyle>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct CalloutBlock {
    #[serde(default)]
    pub background_color: i32,
    #[serde(default)]
    pub border_color: i32,
    #[serde(default)]
    pub text_color: i32,
    #[serde(default)]
    pub emoji_id: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct ImageBlock {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub align: i32,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct FileApiBlock {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TableBlock {
    #[serde(default)]
    pub property: Option<TableProperty>,
    #[serde(default)]
    pub cells: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct TableProperty {
    #[serde(default)]
    pub row_size: i32,
    #[serde(default)]
    pub column_size: i32,
    #[serde(default)]
    pub column_width: Vec<i32>,
    #[serde(default)]
    pub header_row: bool,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct GridBlock {
    #[serde(default)]
    pub column_size: i32,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct GridColumnBlock {
    #[serde(default)]
    pub width_ratio: f32,
}

/// Envelope helpers used by the app-side HTTP client.
#[derive(Clone, Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    pub code: i32,
    #[serde(default)]
    pub msg: String,
    /// Missing `data` becomes `None` without requiring `T: Default`.
    pub data: Option<T>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BlocksData {
    #[serde(default)]
    pub items: Vec<ApiBlock>,
    #[serde(default)]
    pub page_token: String,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DocumentData {
    #[serde(default)]
    pub document: Option<DocumentMeta>,
}
