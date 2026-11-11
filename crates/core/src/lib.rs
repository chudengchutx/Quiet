//! quiet.md's document engine, free of any UI: the block model, DocxXML
//! reading and writing, the editing core with undo, the library on disk,
//! and Markdown / HTML / PDF import plus Markdown export.

pub mod docxml;
pub mod editor;
pub mod emoji;
pub mod feishu;
pub mod lang;
pub mod markdown;
pub mod model;
pub mod palette;
pub mod paste;
pub mod pdf;
pub mod store;
pub mod time;
