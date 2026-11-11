//! Import a digital (text-based) PDF as a Quiet.md document.
//!
//! Text is taken in reading order with `pdf-extract` (pure Rust, lopdf).
//! Headings are guessed from font size, lists from leading markers, and
//! embedded raster images become Image blocks. Layout, columns, tables,
//! footnotes and scanned (OCR) pages are out of scope.

use crate::model::{Align, Block, BlockKind, Document, RichText};
use base64::Engine;
use pdf_extract::{content::Content, Dictionary, Document as PdfDoc, MediaBox, Object, ObjectId, OutputDev, OutputError, Stream, Transform};
use std::collections::HashSet;
use std::fmt;
use std::path::Path;

/// Why a PDF could not be imported as a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PdfError {
    /// The file is password-protected.
    Encrypted,
    /// No extractable text (a scan, or an empty / image-only file).
    NoText,
    /// Damaged, unreadable, or not a PDF.
    Unreadable,
}

impl PdfError {
    pub fn message(&self) -> &'static str {
        match self {
            PdfError::Encrypted => "无法导入加密的 PDF，请先解除密码后再试",
            PdfError::NoText => "这是扫描件或没有可提取文字的 PDF，当前不支持识别",
            PdfError::Unreadable => "PDF 已损坏或无法读取",
        }
    }
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PdfError {}

/// A PDF file as a page: titled after the file, its leading heading dropped
/// when it repeats the name (same as Markdown import).
pub fn document_from_pdf(path: &Path) -> Result<Document, PdfError> {
    let bytes = std::fs::read(path).map_err(|_| PdfError::Unreadable)?;
    document_from_bytes(&bytes, path.file_stem().and_then(|s| s.to_str()).unwrap_or_default())
}

/// Like `document_from_pdf`, with an explicit title stem (the file name).
pub fn document_from_bytes(bytes: &[u8], stem: &str) -> Result<Document, PdfError> {
    let mut doc = PdfDoc::load_mem(bytes).map_err(map_load_err)?;
    if doc.is_encrypted() {
        // Some "encrypted" files use an empty user password.
        if doc.decrypt("").is_err() {
            return Err(PdfError::Encrypted);
        }
    }
    let mut collector = Collector::default();
    let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pdf_extract::output_doc(&doc, &mut collector)));
    match extracted {
        Ok(Ok(())) => {}
        _ => return Err(PdfError::Unreadable),
    }
    let images = extract_images(&doc);
    if collector.glyphs.iter().all(|g| g.ch.trim().is_empty()) {
        return Err(PdfError::NoText);
    }
    let mut blocks = blocks_from(&collector.glyphs, &images);
    let stem = stem.trim();
    if blocks.first().is_some_and(|b| matches!(b.kind, BlockKind::Heading { level: 1, .. }) && b.text.text.trim() == stem) {
        blocks.remove(0);
    }
    if blocks.is_empty() {
        blocks.push(Block::text(""));
    }
    Ok(Document { title: RichText::plain(stem), icon: None, cover: None, blocks })
}

fn map_load_err(e: pdf_extract::Error) -> PdfError {
    use pdf_extract::Error::*;
    match e {
        AlreadyEncrypted | InvalidPassword | UnsupportedSecurityHandler(_) | Decryption(_) => PdfError::Encrypted,
        _ => PdfError::Unreadable,
    }
}

struct Glyph {
    page: u32,
    x: f64,
    y: f64,
    size: f64,
    end: f64,
    ch: String,
}

struct PlacedImage {
    page: u32,
    x: f64,
    y: f64,
    src: String,
}

#[derive(Default)]
struct Collector {
    glyphs: Vec<Glyph>,
    flip: Transform,
    page: u32,
}

impl OutputDev for Collector {
    fn begin_page(&mut self, page_num: u32, media_box: &MediaBox, _: Option<(f64, f64, f64, f64)>) -> Result<(), OutputError> {
        self.page = page_num;
        self.flip = Transform::row_major(1., 0., 0., -1., 0., media_box.ury - media_box.lly);
        Ok(())
    }
    fn end_page(&mut self) -> Result<(), OutputError> { Ok(()) }
    fn output_character(&mut self, trm: &Transform, width: f64, _spacing: f64, font_size: f64, char: &str) -> Result<(), OutputError> {
        if char.is_empty() { return Ok(()); }
        let position = trm.post_transform(&self.flip);
        let (sx, sy) = (trm.m11 * font_size + trm.m21 * font_size, trm.m12 * font_size + trm.m22 * font_size);
        let size = (sx.abs() * sy.abs()).sqrt();
        let size = if size.is_finite() && size > 0.1 { size } else { font_size.max(1.) };
        let (x, y) = (position.m31, position.m32);
        self.glyphs.push(Glyph { page: self.page, x, y, size, end: x + width * size, ch: char.to_string() });
        Ok(())
    }
    fn begin_word(&mut self) -> Result<(), OutputError> { Ok(()) }
    fn end_word(&mut self) -> Result<(), OutputError> { Ok(()) }
    fn end_line(&mut self) -> Result<(), OutputError> { Ok(()) }
}

enum Item {
    Line { page: u32, x: f64, y: f64, size: f64, text: String },
    Image { page: u32, x: f64, y: f64, src: String },
}

impl Item {
    fn key(&self) -> (u32, i64, i64) {
        let (p, y, x) = match self {
            Item::Line { page, x, y, .. } | Item::Image { page, x, y, .. } => (*page, *y, *x),
        };
        (p, (y * 10.) as i64, (x * 10.) as i64)
    }
}

fn blocks_from(glyphs: &[Glyph], images: &[PlacedImage]) -> Vec<Block> {
    let mut items: Vec<Item> = lines_from(glyphs).into_iter().map(|(page, x, y, size, text)| Item::Line { page, x, y, size, text }).collect();
    items.extend(images.iter().map(|im| Item::Image { page: im.page, x: im.x, y: im.y, src: im.src.clone() }));
    items.sort_by_key(|i| i.key());

    // Body size: the size that covers the most characters.
    let mut counts: Vec<(i32, usize)> = Vec::new();
    for g in glyphs {
        if g.ch.trim().is_empty() { continue; }
        let k = (g.size * 4.).round() as i32;
        if let Some(c) = counts.iter_mut().find(|(s, _)| *s == k) { c.1 += g.ch.chars().count(); } else { counts.push((k, g.ch.chars().count())); }
    }
    counts.sort_by_key(|(_, n)| *n);
    let body = counts.last().map(|(s, _)| *s as f64 / 4.).filter(|s| *s > 0.).unwrap_or(12.);
    let heading_ranks = heading_sizes(&items, body);

    let mut blocks = Vec::new();
    let mut para = String::new();
    let mut para_size = 0.;
    let mut para_x = 0.;
    let mut para_y = 0.;
    let mut para_page = 0u32;
    let flush = |blocks: &mut Vec<Block>, para: &mut String, size: f64, body: f64, ranks: &[(i32, u8)]| {
        let t = para.trim();
        if t.is_empty() { para.clear(); return; }
        blocks.push(block_for(t, size, body, ranks));
        para.clear();
    };
    for item in items {
        match item {
            Item::Image { src, .. } => {
                flush(&mut blocks, &mut para, para_size, body, &heading_ranks);
                blocks.push(image_block(src));
            }
            Item::Line { page, x, y, size, text } => {
                let kind = classify_line(&text);
                let heading = heading_level(size, body, &heading_ranks).is_some();
                if kind.is_some() || heading {
                    flush(&mut blocks, &mut para, para_size, body, &heading_ranks);
                    blocks.push(block_for(&text, size, body, &heading_ranks));
                    continue;
                }
                if para.is_empty() {
                    para = text;
                    para_size = size;
                    para_x = x;
                    para_y = y;
                    para_page = page;
                    continue;
                }
                let gap = y - para_y;
                let size_ok = (size - para_size).abs() <= para_size * 0.15;
                let indent_ok = (x - para_x).abs() <= para_size * 2.5;
                let close = gap <= para_size * 1.55;
                let new_para = page != para_page || !size_ok || !close || (!indent_ok && gap > para_size * 0.9) || sentence_break(&para, gap, para_size);
                if new_para {
                    flush(&mut blocks, &mut para, para_size, body, &heading_ranks);
                    para = text;
                    para_size = size;
                    para_x = x;
                    para_y = y;
                    para_page = page;
                } else {
                    if !para.ends_with(char::is_whitespace) && !text.starts_with(char::is_whitespace) {
                        // PDF line wraps usually have no trailing space.
                        if needs_latin_space(&para, &text) { para.push(' '); }
                    }
                    para.push_str(&text);
                    para_y = y;
                }
            }
        }
    }
    flush(&mut blocks, &mut para, para_size, body, &heading_ranks);
    blocks
}

fn needs_latin_space(prev: &str, next: &str) -> bool {
    let a = prev.chars().rev().find(|c| !c.is_whitespace());
    let b = next.chars().find(|c| !c.is_whitespace());
    match (a, b) {
        (Some(a), Some(b)) => a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric(),
        _ => false,
    }
}

fn sentence_break(prev: &str, gap: f64, size: f64) -> bool {
    if gap <= size * 0.9 { return false; }
    prev.trim_end().ends_with(['。', '！', '？', '.', '!', '?', '…'])
}

fn heading_sizes(items: &[Item], body: f64) -> Vec<(i32, u8)> {
    let mut sizes: Vec<i32> = items.iter().filter_map(|i| match i {
        Item::Line { size, text, .. } if !text.trim().is_empty() && *size >= body * 1.18 => Some((*size * 4.).round() as i32),
        _ => None,
    }).collect();
    sizes.sort_by(|a, b| b.cmp(a));
    sizes.dedup();
    sizes.into_iter().take(4).enumerate().map(|(i, s)| (s, (i as u8) + 1)).collect()
}

fn heading_level(size: f64, body: f64, ranks: &[(i32, u8)]) -> Option<u8> {
    if size < body * 1.18 { return None; }
    let k = (size * 4.).round() as i32;
    ranks.iter().find(|(s, _)| (k - *s).abs() <= 1).map(|(_, l)| *l).or(Some(1))
}

fn block_for(text: &str, size: f64, body: f64, ranks: &[(i32, u8)]) -> Block {
    let text = text.trim();
    if let Some((kind, rest)) = classify_line(text) {
        return Block::new(kind, RichText::plain(rest));
    }
    if let Some(level) = heading_level(size, body, ranks) {
        return Block::new(BlockKind::Heading { level, seq_auto: false }, RichText::plain(text));
    }
    Block::text(text)
}

fn classify_line(text: &str) -> Option<(BlockKind, &str)> {
    let t = text.trim();
    if let Some(rest) = strip_bullet(t) { return Some((BlockKind::Bullet, rest)); }
    if let Some((n, rest)) = strip_number(t) {
        return Some((BlockKind::Ordered { seq: (n != 1).then_some(n) }, rest));
    }
    None
}

fn strip_bullet(t: &str) -> Option<&str> {
    const MARKS: &[char] = &['•', '·', '●', '○', '◦', '▪', '▫', '‣', '▸', '–', '—', '-', '*', '·'];
    let mut chars = t.chars();
    let first = chars.next()?;
    if !MARKS.contains(&first) { return None; }
    let rest = chars.as_str();
    if rest.starts_with(char::is_whitespace) { return Some(rest.trim_start()); }
    // A lone dash/asterisk glued to a word is not a list marker.
    if matches!(first, '-' | '*' | '–' | '—') { return None; }
    Some(rest.trim_start())
}

fn strip_number(t: &str) -> Option<(u32, &str)> {
    let bytes = t.as_bytes();
    if bytes.first() == Some(&b'(') || t.starts_with('（') {
        let inner = if bytes.first() == Some(&b'(') { &t[1..] } else { t.trim_start_matches('（') };
        let n_end = inner.find(|c: char| !c.is_ascii_digit())?;
        if n_end == 0 { return None; }
        let n: u32 = inner[..n_end].parse().ok()?;
        let after = &inner[n_end..];
        let rest = after.strip_prefix(')').or_else(|| after.strip_prefix('）'))?;
        if !rest.starts_with(char::is_whitespace) && !rest.is_empty() && n_end + 1 == inner.len() { return None; }
        return Some((n, rest.trim_start()));
    }
    let n_end = t.find(|c: char| !c.is_ascii_digit())?;
    if n_end == 0 { return None; }
    let n: u32 = t[..n_end].parse().ok()?;
    let after = &t[n_end..];
    let rest = after.strip_prefix('.').or_else(|| after.strip_prefix('、')).or_else(|| after.strip_prefix(')'))?;
    if rest.starts_with(char::is_whitespace) || rest.is_empty() { return Some((n, rest.trim_start())); }
    None
}

fn image_block(src: String) -> Block {
    let mut b = Block::new(
        BlockKind::Image { src_kind: "src".into(), src, width: None, height: None, caption: None, crop: None },
        RichText::default(),
    );
    b.align = Align::Center;
    b
}

fn lines_from(glyphs: &[Glyph]) -> Vec<(u32, f64, f64, f64, String)> {
    let mut glyphs: Vec<&Glyph> = glyphs.iter().filter(|g| g.ch != "\u{0}").collect();
    glyphs.sort_by(|a, b| a.page.cmp(&b.page).then((a.y * 20.).round().total_cmp(&(b.y * 20.).round())).then(a.x.total_cmp(&b.x)));
    let mut lines = Vec::new();
    let mut i = 0;
    while i < glyphs.len() {
        let g0 = glyphs[i];
        let mut line = vec![g0];
        let mut y = g0.y;
        let mut end = g0.end;
        i += 1;
        while i < glyphs.len() {
            let g = glyphs[i];
            if g.page != g0.page { break; }
            let tol = g0.size.max(g.size) * 0.4;
            if (g.y - y).abs() > tol { break; }
            line.push(g);
            y = (y * (line.len() - 1) as f64 + g.y) / line.len() as f64;
            end = g.end.max(end);
            i += 1;
        }
        line.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut text = String::new();
        let mut last_end = line[0].x;
        let mut x0 = line[0].x;
        let mut size = 0.;
        let mut size_n = 0.;
        for g in &line {
            if !text.is_empty() && g.x > last_end + g.size * 0.12 { text.push(' '); }
            text.push_str(&g.ch);
            last_end = g.end.max(g.x);
            x0 = x0.min(g.x);
            if !g.ch.trim().is_empty() { size += g.size; size_n += 1.; }
        }
        let size = if size_n > 0. { size / size_n } else { g0.size };
        let text = text.trim().to_string();
        if !text.is_empty() {
            lines.push((g0.page, x0, line.iter().map(|g| g.y).sum::<f64>() / line.len() as f64, size, text));
        }
    }
    lines
}

fn extract_images(doc: &PdfDoc) -> Vec<PlacedImage> {
    let mut out = Vec::new();
    let pages = doc.get_pages();
    for (page_no, page_id) in pages {
        let boxy = page_media_box(doc, page_id).unwrap_or((0., 0., 612., 792.));
        let flip_h = boxy.3 - boxy.1;
        let res = inherited_xobjects(doc, page_id);
        let Ok(bytes) = doc.get_page_content(page_id) else { continue };
        let Ok(content) = Content::decode(&bytes) else { continue };
        walk_ops(doc, &content.operations, &res, Transform::identity(), flip_h, page_no, 0, &mut HashSet::new(), &mut out);
    }
    out
}

fn page_media_box(doc: &PdfDoc, mut id: ObjectId) -> Option<(f64, f64, f64, f64)> {
    loop {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(obj) = dict.get(b"MediaBox")
            && let Some(b) = as_box(doc, obj)
        {
            return Some(b);
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(p)) => id = *p,
            _ => return None,
        }
    }
}

fn as_box(doc: &PdfDoc, obj: &Object) -> Option<(f64, f64, f64, f64)> {
    let arr = match obj {
        Object::Array(a) => a,
        Object::Reference(id) => doc.get_object(*id).ok()?.as_array().ok()?,
        _ => return None,
    };
    if arr.len() < 4 { return None; }
    Some((as_f64(&arr[0])?, as_f64(&arr[1])?, as_f64(&arr[2])?, as_f64(&arr[3])?))
}

fn inherited_xobjects(doc: &PdfDoc, mut id: ObjectId) -> Dictionary {
    let mut acc = Dictionary::new();
    while let Ok(dict) = doc.get_dictionary(id) {
        if let Some(x) = resources_xobjects(doc, dict) {
            for (k, v) in x.iter() {
                if acc.get(k).is_err() { acc.set(k.clone(), v.clone()); }
            }
        }
        match dict.get(b"Parent") {
            Ok(Object::Reference(p)) => id = *p,
            _ => break,
        }
    }
    acc
}

fn resources_xobjects<'a>(doc: &'a PdfDoc, dict: &'a Dictionary) -> Option<&'a Dictionary> {
    let res = deref_dict(doc, dict.get(b"Resources").ok()?)?;
    deref_dict(doc, res.get(b"XObject").ok()?)
}

fn deref_dict<'a>(doc: &'a PdfDoc, obj: &'a Object) -> Option<&'a Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

fn deref_stream<'a>(doc: &'a PdfDoc, obj: &'a Object) -> Option<&'a Stream> {
    match obj {
        Object::Stream(s) => Some(s),
        Object::Reference(id) => match doc.get_object(*id).ok()? {
            Object::Stream(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}

fn walk_ops(
    doc: &PdfDoc,
    ops: &[pdf_extract::content::Operation],
    xobjects: &Dictionary,
    mut ctm: Transform,
    flip_h: f64,
    page: u32,
    depth: u8,
    seen: &mut HashSet<ObjectId>,
    out: &mut Vec<PlacedImage>,
) {
    if depth > 16 { return; }
    let mut stack = Vec::new();
    for op in ops {
        match op.operator.as_str() {
            "q" => stack.push(ctm),
            "Q" => { if let Some(m) = stack.pop() { ctm = m; } }
            "cm" => { if let Some(m) = matrix6(&op.operands) { ctm = ctm.post_transform(&m); } }
            "Do" => {
                let Some(name) = op.operands.first().and_then(as_name) else { continue };
                let Some(obj) = xobjects.get(name).ok() else { continue };
                let Some(stream) = deref_stream(doc, obj) else { continue };
                let subtype = stream.dict.get(b"Subtype").ok().and_then(as_name).unwrap_or(b"");
                if subtype == b"Image" {
                    if let Some((bytes, mime)) = encode_image(stream) {
                        if let Some((w, h)) = image_dims(stream)
                            && ((w as u32) < 8 || (h as u32) < 8 || (w as u64) * (h as u64) < 64)
                        {
                            continue;
                        }
                        let y = flip_h - (ctm.m32 + ctm.m22.abs() * 0.5);
                        out.push(PlacedImage { page, x: ctm.m31, y, src: data_uri(&bytes, mime) });
                    }
                } else if subtype == b"Form" {
                    let id = match obj {
                        Object::Reference(id) => *id,
                        _ => continue,
                    };
                    if !seen.insert(id) { continue; }
                    let form_res = stream.dict.get(b"Resources").ok().and_then(|r| deref_dict(doc, r))
                        .and_then(|r| r.get(b"XObject").ok().and_then(|x| deref_dict(doc, x)))
                        .cloned()
                        .unwrap_or_else(|| xobjects.clone());
                    let Ok(raw) = stream_bytes(stream) else { seen.remove(&id); continue };
                    if let Ok(inner) = Content::decode(&raw) {
                        let form_ctm = form_matrix(&stream.dict).map(|m| ctm.post_transform(&m)).unwrap_or(ctm);
                        walk_ops(doc, &inner.operations, &form_res, form_ctm, flip_h, page, depth + 1, seen, out);
                    }
                    seen.remove(&id);
                }
            }
            _ => {}
        }
    }
}

fn form_matrix(dict: &Dictionary) -> Option<Transform> {
    let obj = dict.get(b"Matrix").ok()?;
    let arr = obj.as_array().ok()?;
    matrix6(arr)
}

fn matrix6(ops: &[Object]) -> Option<Transform> {
    if ops.len() < 6 { return None; }
    Some(Transform::row_major(as_f64(&ops[0])?, as_f64(&ops[1])?, as_f64(&ops[2])?, as_f64(&ops[3])?, as_f64(&ops[4])?, as_f64(&ops[5])?))
}

fn as_name(obj: &Object) -> Option<&[u8]> {
    match obj {
        Object::Name(n) => Some(n),
        _ => None,
    }
}

fn as_f64(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

fn image_dims(stream: &Stream) -> Option<(i64, i64)> {
    Some((stream.dict.get(b"Width").ok()?.as_i64().ok()?, stream.dict.get(b"Height").ok()?.as_i64().ok()?))
}

fn filters_of(stream: &Stream) -> Vec<String> {
    let Ok(obj) = stream.dict.get(b"Filter") else { return Vec::new() };
    match obj {
        Object::Name(n) => vec![String::from_utf8_lossy(n).into_owned()],
        Object::Array(a) => a.iter().filter_map(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).into_owned()).collect(),
        _ => Vec::new(),
    }
}

fn stream_bytes(stream: &Stream) -> Result<Vec<u8>, ()> {
    match stream.decompressed_content() {
        Ok(b) => Ok(b),
        Err(_) => Ok(stream.content.clone()),
    }
}

fn color_space_name(doc_or_none: Option<&PdfDoc>, stream: &Stream) -> String {
    let Ok(obj) = stream.dict.get(b"ColorSpace") else { return String::new() };
    match obj {
        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
        Object::Array(a) => a.first().and_then(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default(),
        Object::Reference(id) => doc_or_none.and_then(|d| d.get_object(*id).ok()).and_then(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default(),
        _ => String::new(),
    }
}

fn encode_image(stream: &Stream) -> Option<(Vec<u8>, &'static str)> {
    if stream.dict.get(b"ImageMask").ok().and_then(|o| o.as_bool().ok()) == Some(true) {
        return None;
    }
    let filters = filters_of(stream);
    let data = stream_bytes(stream).ok()?;
    if data.starts_with(&[0xFF, 0xD8]) {
        return Some((data, "image/jpeg"));
    }
    if filters.iter().any(|f| matches!(f.as_str(), "JPXDecode" | "JBIG2Decode" | "CCITTFaxDecode" | "CCITTFax")) {
        return None;
    }
    let (w, h) = image_dims(stream)?;
    if w <= 0 || h <= 0 { return None; }
    let bpc = stream.dict.get(b"BitsPerComponent").ok().and_then(|o| o.as_i64().ok()).unwrap_or(8);
    if bpc != 8 { return None; }
    let cs = color_space_name(None, stream);
    png_from_samples(&data, w as u32, h as u32, &cs)
}

fn png_from_samples(data: &[u8], w: u32, h: u32, cs: &str) -> Option<(Vec<u8>, &'static str)> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};
    let pixels = w as usize * h as usize;
    let components = match cs {
        "DeviceGray" | "CalGray" | "G" => 1,
        "DeviceRGB" | "CalRGB" | "RGB" => 3,
        "DeviceCMYK" | "CMYK" => 4,
        _ => match data.len().checked_div(pixels)? { 1 | 3 | 4 => data.len() / pixels, _ => return None },
    };
    let expected = pixels.checked_mul(components)?;
    if data.len() < expected { return None; }
    let samples = &data[..expected];
    let (buf, color) = match components {
        1 => (samples.to_vec(), ExtendedColorType::L8),
        3 => (samples.to_vec(), ExtendedColorType::Rgb8),
        4 => (cmyk_to_rgb(samples), ExtendedColorType::Rgb8),
        _ => return None,
    };
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive).write_image(&buf, w, h, color).ok()?;
    Some((out, "image/png"))
}

fn cmyk_to_rgb(samples: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() / 4 * 3);
    for px in samples.as_chunks::<4>().0 {
        let (c, m, y, k) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
        out.push(((255 - c) * (255 - k) / 255) as u8);
        out.push(((255 - m) * (255 - k) / 255) as u8);
        out.push(((255 - y) * (255 - k) / 255) as u8);
    }
    out
}

fn data_uri(bytes: &[u8], mime: &str) -> String {
    format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_extract::{content::{Content, Operation}, dictionary, Object, Stream};
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("fdl-pdf-{name}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }
        fn path(&self, name: &str) -> PathBuf { self.0.join(name) }
    }
    impl Drop for Tmp {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn ops(items: Vec<Operation>) -> Vec<u8> { Content { operations: items }.encode().unwrap() }

    fn save_pdf(path: &Path, content: Vec<u8>, xobjects: Option<Dictionary>) {
        let mut doc = PdfDoc::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let mut resources = dictionary! { "Font" => dictionary! { "F1" => font_id } };
        if let Some(xo) = xobjects { resources.set("XObject", xo); }
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).unwrap();
    }

    fn text_op(size: i64, x: i64, y: i64, s: &str) -> Vec<Operation> {
        vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), size.into()]),
            Operation::new("Td", vec![x.into(), y.into()]),
            Operation::new("Tj", vec![Object::string_literal(s)]),
            Operation::new("ET", vec![]),
        ]
    }

    #[test]
    fn imports_paragraphs_heading_and_list() {
        let tmp = Tmp::new("text");
        let path = tmp.path("Weekly.pdf");
        let mut operations = Vec::new();
        operations.extend(text_op(24, 72, 720, "Weekly"));
        operations.extend(text_op(12, 72, 680, "First paragraph of the body."));
        operations.extend(text_op(12, 72, 640, "1. Alpha"));
        operations.extend(text_op(12, 72, 620, "2. Beta"));
        operations.extend(text_op(12, 72, 590, "- Gamma"));
        save_pdf(&path, ops(operations), None);
        let doc = document_from_pdf(&path).unwrap();
        assert_eq!(doc.title.text, "Weekly");
        // Leading H1 matching the file name is dropped.
        assert!(matches!(doc.blocks[0].kind, BlockKind::Text));
        assert!(doc.blocks[0].text.text.contains("First paragraph"));
        let lists: Vec<_> = doc.blocks.iter().filter(|b| b.kind.is_list()).collect();
        assert!(lists.len() >= 2);
        assert!(matches!(lists[0].kind, BlockKind::Ordered { .. }));
        assert_eq!(lists[0].text.text, "Alpha");
        assert_eq!(lists[1].text.text, "Beta");
        assert!(doc.blocks.iter().any(|b| matches!(b.kind, BlockKind::Bullet) && b.text.text == "Gamma"));
    }

    #[test]
    fn heading_is_kept_when_it_differs_from_filename() {
        let tmp = Tmp::new("head");
        let path = tmp.path("notes.pdf");
        let mut operations = Vec::new();
        operations.extend(text_op(24, 72, 720, "A Big Title"));
        operations.extend(text_op(12, 72, 680, "Body copy here."));
        save_pdf(&path, ops(operations), None);
        let doc = document_from_pdf(&path).unwrap();
        assert_eq!(doc.title.text, "notes");
        assert!(matches!(doc.blocks[0].kind, BlockKind::Heading { level: 1, .. }));
        assert_eq!(doc.blocks[0].text.text, "A Big Title");
        assert!(matches!(doc.blocks[1].kind, BlockKind::Text));
        assert!(doc.blocks[1].text.text.contains("Body copy"));
    }

    #[test]
    fn imports_embedded_raster_image() {
        let tmp = Tmp::new("img");
        let path = tmp.path("photo.pdf");
        let mut pix = Vec::with_capacity(8 * 8 * 3);
        for y in 0..8 {
            for x in 0..8 {
                pix.extend_from_slice(&[255, (x * 16) as u8, (y * 16) as u8]);
            }
        }
        let img = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 8,
                "Height" => 8,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
            },
            pix,
        );
        let mut doc = PdfDoc::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding",
        });
        let img_id = doc.add_object(img);
        let mut operations = text_op(12, 72, 700, "A caption above the picture.");
        operations.extend([
            Operation::new("q", vec![]),
            Operation::new("cm", vec![80.into(), 0.into(), 0.into(), 80.into(), 72.into(), 400.into()]),
            Operation::new("Do", vec!["Im1".into()]),
            Operation::new("Q", vec![]),
        ]);
        let content_id = doc.add_object(Stream::new(dictionary! {}, ops(operations)));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
                "XObject" => dictionary! { "Im1" => img_id },
            },
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        }));
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc.save(&path).unwrap();

        let doc = document_from_pdf(&path).unwrap();
        let img = doc.blocks.iter().find(|b| matches!(b.kind, BlockKind::Image { .. })).expect("image block");
        let BlockKind::Image { src, .. } = &img.kind else { panic!() };
        assert!(src.starts_with("data:image/png;base64,"), "{src}");
        let b64 = src.rsplit_once(',').unwrap().1;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"), "expected a PNG, got {} bytes", bytes.len());
        assert!(doc.blocks.iter().any(|b| b.text.text.contains("caption above")));
    }

    #[test]
    fn rejects_empty_or_invalid_pdf() {
        let tmp = Tmp::new("err");
        let blank = tmp.path("blank.pdf");
        save_pdf(&blank, ops(vec![]), None);
        assert_eq!(document_from_pdf(&blank).unwrap_err(), PdfError::NoText);

        let bad = tmp.path("bad.pdf");
        std::fs::write(&bad, b"this is not a pdf").unwrap();
        assert_eq!(document_from_pdf(&bad).unwrap_err(), PdfError::Unreadable);

        let empty = tmp.path("empty.pdf");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(document_from_pdf(&empty).unwrap_err(), PdfError::Unreadable);
    }

    #[test]
    fn rejects_encrypted_pdf() {
        let tmp = Tmp::new("enc");
        let path = tmp.path("secret.pdf");
        // A minimal file whose trailer points at a Standard-handler /Encrypt
        // dict. Load fails the password (or reports the file as encrypted).
        let pdf = b"%PDF-1.4\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n\
4 0 obj\n<< /Filter /Standard /V 1 /R 2 /O (xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx) /U (xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx) /P -4 >>\nendobj\n\
xref\n0 5\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000189 00000 n \n\
trailer\n<< /Size 5 /Root 1 0 R /Encrypt 4 0 R /ID [<0123456789ABCDEF0123456789ABCDEF> <0123456789ABCDEF0123456789ABCDEF>] >>\n\
startxref\n330\n%%EOF\n";
        std::fs::write(&path, pdf).unwrap();
        let err = document_from_pdf(&path).unwrap_err();
        assert!(err == PdfError::Encrypted || err == PdfError::Unreadable, "{err:?}");
        // Prefer Encrypted when the loader can see the /Encrypt dict.
        if let Ok(d) = PdfDoc::load(&path) {
            assert!(d.is_encrypted());
            assert_eq!(document_from_pdf(&path).unwrap_err(), PdfError::Encrypted);
        }
    }
}
