//! Feishu / Lark document link import: URL parse and Open API blocks → Document.
//! Network and OAuth live in the app crate; this module stays GPUI-free.

pub mod map;
pub mod types;
pub mod url;

pub use map::{document_from_blocks, label_for_raw_tag, raw_card_info, unsupported_label, MapOptions};
pub use types::{ApiBlock, DocumentMeta};
pub use url::{parse_doc_url, DocRef, DocUrlError};
