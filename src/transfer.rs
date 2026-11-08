//! 导入 / 导出: 下载为 → PDF / Markdown (with the 导出 Markdown 设置
//! dialog), 打印, and 导入为文档 (主页's 上传, the app menu): local Markdown
//! or DocxXML files picked straight away. The Word / PDF settings and
//! 打印选项 only choose what happens to comments, so they are left out.

use crate::app::Workspace;
use crate::settings::display_title;
use crate::model::{Block, BlockKind, Document, RichText};
use crate::printing::{self, Job};
use crate::theme::Theme;
use crate::trash::{close_button, dialog, primary_button, scrim, secondary_button};
use crate::ui::ToastKind;
use gpui::{prelude::FluentBuilder, *};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The open 导出 Markdown 设置 dialog: the page, and 所有内容 picked.
pub struct ExportMd {
    pub id: String,
    pub all: bool,
}

/// 从飞书链接导入 dialog state.
pub struct ImportLink {
    pub url: String,
    pub busy: bool,
    pub error: Option<String>,
    /// Live stage line while `busy` (Chinese source; localized on render).
    pub status: String,
}

/// What to do after the 坚果云账号 dialog saves credentials.
#[derive(Clone, Debug)]
pub enum NutstorePending {
    Upload(Vec<String>),
    Import,
}

/// 坚果云账号设置 dialog: fill in, then continue `pending`.
pub struct NutstoreAuthDlg {
    pub pending: NutstorePending,
    pub error: Option<String>,
}

/// Pick remote zip packages to import.
pub struct NutstoreImportDlg {
    /// `(file name, selected)`.
    pub items: Vec<(String, bool)>,
    pub busy: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub status: String,
}

const MARKDOWN_EXTS: [&str; 4] = ["md", "markdown", "mark", "txt"];

fn downloads() -> PathBuf {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_default()
}

/// Where to save `name`: the system save panel (Downloads first), or a
/// scripted answer.
fn save_path(name: &str, cx: &mut App) -> impl std::future::Future<Output = Option<PathBuf>> + use<> {
    let picked = crate::devtools::take_pick().and_then(|p| p.into_iter().next());
    let prompt = picked.is_none().then(|| cx.prompt_for_new_path(&downloads(), Some(name)));
    async move {
        match prompt {
            Some(p) => p.await.ok().and_then(Result::ok).flatten(),
            None => picked,
        }
    }
}

/// Markdown and DocxXML files to import: the system open panel, or a
/// scripted answer.
fn open_paths(cx: &mut App) -> impl std::future::Future<Output = Option<Vec<PathBuf>>> + use<> {
    let picked = crate::devtools::take_pick();
    let extensions = Some(MARKDOWN_EXTS.iter().chain(&["xml"]).map(|e| SharedString::from(*e)).collect());
    let prompt = picked.is_none().then(|| cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(crate::settings::text("导入").into()), extensions }));
    async move {
        match prompt {
            Some(p) => p.await.ok().and_then(Result::ok).flatten(),
            None => picked,
        }
    }
}

/// The page title as a file name.
fn file_name(doc: &Document) -> String {
    display_title(&doc.title.text).replace(['/', ':', '\\'], "_")
}

/// The modal mask (the window dimmed) and its dialog.
fn mask(id: &'static str, dialog: impl IntoElement, close: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static, cx: &mut Context<Workspace>) -> AnyElement {
    deferred(
        div()
            .id(id)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(scrim())
            .on_mouse_down(MouseButton::Left, cx.listener(move |w, _, _, cx| close(w, cx)))
            .child(dialog),
    )
    .with_priority(3)
    .into_any_element()
}

/// 16px radio: the accent with a white dot when checked, else a white disc
/// with the task box's rim.
pub(crate) fn radio(on: bool) -> Div {
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .when(on, |d| d.bg(Theme::primary()).child(div().size(px(6.)).rounded_full().bg(crate::theme::raised(Theme::bg_body()))))
        .when(!on, |d| d.border_1().border_color(Theme::todo_box()).bg(crate::theme::raised(Theme::bg_body())))
}

/// Percent-escapes in a Markdown link (`%20` and friends).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Relative and `file://` image links of an imported file, made absolute.
fn resolve_images(blocks: &mut [Block], base: &Path) {
    for b in blocks.iter_mut() {
        resolve_images(&mut b.children, base);
        if let BlockKind::Table(t) = &mut b.kind {
            for cell in t.rows.iter_mut().flatten() {
                resolve_images(&mut cell.blocks, base);
            }
        }
        let BlockKind::Image { src, .. } = &mut b.kind else { continue };
        if src.starts_with("http://") || src.starts_with("https://") || src.starts_with("data:") {
            continue;
        }
        let path = match src.strip_prefix("file://") {
            Some(p) => PathBuf::from(percent_decode(p)),
            None => base.join(percent_decode(src)),
        };
        *src = path.to_string_lossy().to_string();
    }
}

/// A Markdown / TXT file as a page: titled after the file, its leading
/// `# 标题` dropped when it repeats the name.
pub fn markdown_document(path: &Path) -> std::io::Result<Document> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().trim().to_string();
    let off = crate::paste::Off { file: true, ..Default::default() };
    let mut blocks = crate::paste::markdown_to_blocks(text, off);
    if blocks.first().is_some_and(|b| matches!(b.kind, BlockKind::Heading { level: 1, .. }) && b.text.text.trim() == stem) {
        blocks.remove(0);
    }
    if let Some(dir) = path.parent() {
        resolve_images(&mut blocks, dir);
    }
    crate::editor::image::localize_pasted_images(&mut blocks);
    if blocks.is_empty() {
        blocks.push(Block::text(""));
    }
    Ok(Document { title: RichText::plain(stem), icon: None, cover: None, blocks })
}

impl Workspace {
    fn close_menus(&mut self) {
        self.doc_menu = false;
        self.doc_sub = None;
        self.row_menu = None;
    }

    /// Titles of `id`'s child pages, for its 子页面列表 blocks.
    fn sub_page_titles(&self, id: &str) -> Vec<String> {
        crate::tree::Tree::new(&self.entries).children(Some(id)).iter().map(|e| e.title.clone()).collect()
    }

    /// 下载为 → Markdown: the 导出 Markdown 设置 first.
    pub fn open_export_md(&mut self, id: &str, cx: &mut Context<Self>) {
        self.close_menus();
        self.export_md = Some(ExportMd { id: id.to_string(), all: false });
        cx.notify();
    }

    /// 导出: the .md where the user picks; 所有内容 copies local images into
    /// `<name>.assets` beside it.
    fn export_markdown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ExportMd { id, all }) = self.export_md.take() else { return };
        cx.notify();
        let Some(doc) = self.doc_of(&id, cx) else { return };
        let subs = self.sub_page_titles(&id);
        let path = save_path(&format!("{}.md", file_name(&doc)), cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = path.await else { return };
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("doc").to_string();
            let assets = format!("{stem}.assets");
            let out = crate::markdown::export(&doc, &crate::markdown::Options { sub_pages: &subs, assets: all.then_some(assets.as_str()) });
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut missing = 0;
            let written = std::fs::write(&path, &out.text).and_then(|_| {
                for (src, rel) in &out.files {
                    let to = dir.join(rel);
                    if let Some(p) = to.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    // A missing image only loses itself, but is reported.
                    if std::fs::copy(src, &to).is_err() {
                        missing += 1;
                    }
                }
                Ok(())
            });
            if written.is_err() {
                let _ = this.update(cx, |w, cx| w.show_toast_kind(ToastKind::Error, crate::settings::text("导出失败，请重试"), cx));
            } else if missing > 0 {
                let _ = this.update(cx, |w, cx| w.show_toast_kind(ToastKind::Error, &crate::localized_format!("已导出，但有 {missing} 个图片或附件未能复制", "Exported, but {missing} images or attachments could not be copied"), cx));
            }
        })
        .detach();
    }

    /// 下载为 → PDF: the page laid out on A4 by WebKit, saved where the user picks.
    pub fn export_pdf(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus();
        if !printing::SUPPORTED {
            self.show_toast_kind(ToastKind::Error, crate::settings::text("当前系统不支持"), cx);
            return;
        }
        cx.notify();
        let Some(doc) = self.doc_of(id, cx) else { return };
        let subs = self.sub_page_titles(id);
        let name = file_name(&doc);
        let path = save_path(&format!("{name}.pdf"), cx);
        let view = printing::ns_view_of(window);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = path.await else { return };
            let _ = this.update(cx, |w, cx| w.show_toast_kind(ToastKind::Info, crate::settings::text("正在导出文档"), cx));
            let html = crate::html::to_html(&doc, &crate::html::Options { sub_pages: &subs });
            let _ = std::fs::remove_file(&path);
            let res = printing::run(html, name, Job::Pdf(path.clone()), view, cx).await;
            let ok = matches!(res, Ok(true)) && path.is_file();
            let _ = this.update(cx, |w, cx| {
                if ok {
                    w.toast = None;
                    cx.notify();
                } else {
                    w.show_toast_kind(ToastKind::Error, crate::settings::text("导出失败，请重试"), cx);
                }
            });
        })
        .detach();
    }

    /// 打印: the system print panel over the same A4 layout as PDF.
    pub fn print_doc(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus();
        if !printing::SUPPORTED {
            self.show_toast_kind(ToastKind::Error, crate::settings::text("当前系统不支持"), cx);
            return;
        }
        let Some(doc) = self.doc_of(id, cx) else { return };
        let subs = self.sub_page_titles(id);
        let name = file_name(&doc);
        let html = crate::html::to_html(&doc, &crate::html::Options { sub_pages: &subs });
        let view = printing::ns_view_of(window);
        self.show_toast_kind(ToastKind::Info, crate::settings::text("文档打印中"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let res = printing::run(html, name, Job::Print, view, cx).await;
            let _ = this.update(cx, |w, cx| match res {
                Ok(_) => {
                    w.toast = None;
                    cx.notify();
                }
                Err(e) => w.show_toast_kind(ToastKind::Error, &e, cx),
            });
        })
        .detach();
    }

    /// 上传 / 导入为文档: pick Markdown or DocxXML files, each becoming a
    /// document; the last one opens.
    pub fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus();
        cx.notify();
        let paths = open_paths(cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(paths) = paths.await else { return };
            let _ = this.update_in(cx, |w, window, cx| w.import_files(&paths, window, cx));
        })
        .detach();
    }

    fn import_files(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let again = paths.to_vec();
        if !self.save_before(cx, move |w, window, cx| w.import_files(&again, window, cx)) {
            return;
        }
        let mut last = None;
        let mut failed = false;
        for p in paths {
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default().to_ascii_lowercase();
            let made = match ext.as_str() {
                e if MARKDOWN_EXTS.contains(&e) => markdown_document(p).map_err(anyhow::Error::from).and_then(|doc| self.store.create_from(&doc)),
                "xml" => self.store.import(p),
                _ => Err(anyhow::anyhow!("文件格式不支持")),
            };
            match made {
                Ok(id) => last = Some(id),
                Err(_) => failed = true,
            }
        }
        self.entries = self.store.list();
        self.sort();
        if let Some(id) = last {
            self.open(&id, window, cx);
        }
        if failed {
            self.show_toast_kind(ToastKind::Error, crate::settings::text("导入失败，请检查文件格式"), cx);
        }
        cx.notify();
    }

    /// 导出 Markdown 设置 as a modal: white, radius 16, a 15pt semibold
    /// title over a hairline, 13pt options, the buttons on the right.
    pub(crate) fn render_export_md(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let all = self.export_md.as_ref()?.all;
        let option = |id: &'static str, on: bool, label: &'static str, value: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .cursor_pointer()
                .child(radio(on))
                .child(div().ml(px(8.)).child(label))
                .on_click(cx.listener(move |w, _, _, cx| {
                    if let Some(e) = w.export_md.as_mut() {
                        e.all = value;
                        cx.notify();
                    }
                }))
        };
        let dialog = dialog("export-md")
            .relative()
            .w(px(720.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .pt(px(24.))
                    .pb(px(24.))
                    .pl(px(24.))
                    .pr(px(56.))
                    .border_b_1()
                    .border_color(Theme::line_divider())
                    .text_size(px(15.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::settings::text("导出 Markdown 设置")),
            )
            .child(close_button("export-md-close").absolute().top(px(27.)).right(px(25.)).on_click(cx.listener(|w, _, _, cx| {
                w.export_md = None;
                cx.notify();
            })))
            .child(
                div().pt(px(24.)).px(px(24.)).pb(px(16.)).mb(px(24.)).child(
                    div()
                        .flex()
                        .items_center()
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .child(div().mr(px(40.)).text_color(Theme::text_caption()).child(crate::settings::text("内容范围")))
                        .child(
                            div()
                                .flex()
                                .gap(px(24.))
                                .child(option("export-md-text", !all, crate::settings::text("仅文本（不含图片和附件）"), false))
                                .child(option("export-md-all", all, crate::settings::text("所有内容"), true)),
                        ),
                ),
            )
            .child(
                div()
                    .px(px(24.))
                    .pb(px(24.))
                    .flex()
                    .justify_end()
                    .gap(px(12.))
                    .child(secondary_button("export-md-cancel", crate::settings::text("取消")).min_w(px(80.)).on_click(cx.listener(|w, _, _, cx| {
                        w.export_md = None;
                        cx.notify();
                    })))
                    .child(primary_button("export-md-ok", crate::settings::text("导出")).min_w(px(80.)).on_click(cx.listener(|w, _, window, cx| w.export_markdown(window, cx)))),
            );
        Some(mask(
            "export-md-mask",
            dialog,
            |w, cx| {
                w.export_md = None;
                cx.notify();
            },
            cx,
        ))
    }


    /// 从飞书链接导入…
    pub fn open_import_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus();
        let clip = cx.read_from_clipboard()
            .and_then(|item| {
                item.entries().iter().find_map(|e| match e {
                    gpui::ClipboardEntry::String(s) => Some(s.text().clone()),
                    _ => None,
                })
            })
            .unwrap_or_default();
        let url = if fdl_core::feishu::parse_doc_url(&clip).is_ok() { clip } else { String::new() };
        self.import_link = Some(crate::transfer::ImportLink {
            url,
            busy: false,
            error: None,
            status: String::new(),
        });
        if let Some(f) = self.import_link_field.as_ref() {
            f.update(cx, |f, cx| {
                f.set_text(self.import_link.as_ref().map(|i| i.url.as_str()).unwrap_or(""), cx);
            });
        }
        cx.notify();
        let _ = window;
    }

    pub fn confirm_import_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.import_link.as_mut() else { return };
        if state.busy { return; }
        let url = if let Some(f) = self.import_link_field.as_ref() {
            f.read(cx).text.clone()
        } else {
            state.url.clone()
        };
        state.url = url.clone();
        if let Err(e) = fdl_core::feishu::parse_doc_url(&url) {
            state.error = Some(e.to_string());
            cx.notify();
            return;
        }
        state.busy = true;
        state.error = None;
        state.status = crate::settings::text("准备导入…").to_string();
        cx.notify();
        self.show_toast_kind(ToastKind::Info, crate::settings::text("正在导入飞书文档"), cx);
        let root = fdl_core::store::Store::root_dir();
        let assets = fdl_core::store::Store::assets_dir();
        let auth = crate::feishu_import::load_auth(&root);
        // Progress + completion over one channel; curl work stays off the UI thread.
        enum ImportEvent {
            Progress(String),
            Done(Result<Document, String>),
        }
        let (tx, rx) = std::sync::mpsc::channel::<ImportEvent>();
        let tx_progress = tx.clone();
        cx.background_executor()
            .spawn(async move {
                let progress = move |msg: &str| {
                    let _ = tx_progress.send(ImportEvent::Progress(msg.to_string()));
                };
                let result = crate::feishu_import::import_from_url(
                    &url,
                    &auth,
                    &assets,
                    Some(&progress),
                );
                let _ = tx.send(ImportEvent::Done(result.map_err(|e| format!("{e:#}"))));
            })
            .detach();
        cx.spawn_in(window, async move |this, cx| {
            let result = loop {
                let mut done = None;
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        ImportEvent::Progress(msg) => {
                            let _ = this.update(cx, |w, cx| {
                                if let Some(s) = w.import_link.as_mut() {
                                    s.status = msg.clone();
                                }
                                cx.notify();
                            });
                        }
                        ImportEvent::Done(r) => done = Some(r),
                    }
                }
                if let Some(r) = done {
                    break r;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            };
            let _ = this.update_in(cx, |w, window, cx| {
                match result {
                    Ok(doc) => {
                        w.import_link = None;
                        match w.store.create_from(&doc) {
                            Ok(id) => {
                                w.entries = w.store.list();
                                w.sort();
                                w.open(&id, window, cx);
                                w.show_toast_kind(
                                    ToastKind::Info,
                                    crate::settings::text("已从飞书链接导入"),
                                    cx,
                                );
                            }
                            Err(_) => w.show_toast_kind(
                                ToastKind::Error,
                                crate::settings::text("导入失败，请重试"),
                                cx,
                            ),
                        }
                    }
                    Err(e) => {
                        if let Some(s) = w.import_link.as_mut() {
                            s.busy = false;
                            s.status.clear();
                            s.error = Some(e);
                        }
                        w.show_toast_kind(
                            ToastKind::Error,
                            crate::settings::text("飞书链接导入失败"),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_import_link(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.import_link.as_ref()?;
        let busy = state.busy;
        let err = state.error.clone();
        let status = if busy {
            let s = state.status.trim();
            if s.is_empty() {
                crate::settings::text("导入中，请稍候…").to_string()
            } else {
                // Static stage keys localize here; dynamic sheet/media lines are already localized.
                crate::settings::text(s).to_string()
            }
        } else {
            String::new()
        };
        let field = self.import_link_field.clone()?;
        let dialog = dialog("import-feishu")
            .relative()
            .w(px(560.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .pt(px(24.)).pb(px(24.)).pl(px(24.)).pr(px(56.))
                    .border_b_1().border_color(Theme::line_divider())
                    .text_size(px(15.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD)
                    .child(crate::settings::text("从飞书链接导入")),
            )
            .child(close_button("import-feishu-close").absolute().top(px(27.)).right(px(25.)).on_click(cx.listener(|w, _, _, cx| {
                w.import_link = None;
                cx.notify();
            })))
            .child(
                div().pt(px(20.)).px(px(24.)).pb(px(8.))
                    .child(div().text_size(px(13.)).text_color(Theme::text_caption()).mb(px(8.)).child(crate::settings::text("粘贴飞书文档或知识库链接")))
                    .child(
                        div().h(px(36.)).px(px(10.)).rounded(px(6.)).border_1().border_color(Theme::line_divider())
                            .flex().items_center()
                            .child(field.clone())
                    )
                    .when(busy && !status.is_empty(), |d| {
                        d.child(
                            div()
                                .mt(px(8.))
                                .text_size(px(12.))
                                .text_color(Theme::text_caption())
                                .child(status),
                        )
                    })
                    .when_some(err, |d, e| d.child(div().mt(px(8.)).text_size(px(12.)).text_color(Theme::text_caption()).child(e)))
            )
            .child(
                div().px(px(24.)).pb(px(24.)).pt(px(16.)).flex().justify_end()
                    .child(secondary_button("import-feishu-cancel", crate::settings::text("取消")).min_w(px(80.)).mr(px(12.)).on_click(cx.listener(|w, _, _, cx| {
                        w.import_link = None; cx.notify();
                    })))
                    .child(primary_button("import-feishu-ok", if busy { crate::settings::text("导入中…") } else { crate::settings::text("导入") }).min_w(px(80.)).on_click(cx.listener(|w, _, window, cx| {
                        w.confirm_import_link(window, cx);
                    })))
            );
        Some(mask("import-feishu-mask", dialog, |w, cx| { w.import_link = None; cx.notify(); }, cx))
    }

    /// 上传到坚果云: single or batch. Opens auth dialog when credentials are missing.
    pub fn upload_to_nutstore(&mut self, ids: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus();
        self.row_menu = None;
        if ids.is_empty() {
            return;
        }
        // Persist the open doc so the zip matches what the user sees.
        if let Some(cur) = self.current.clone() {
            if ids.iter().any(|id| id == &cur) {
                self.flush(cx);
            }
        }
        let root = fdl_core::store::Store::root_dir();
        let auth = crate::nutstore::load_auth(&root);
        if !auth.is_ready() {
            self.open_nutstore_auth(NutstorePending::Upload(ids), cx);
            return;
        }
        self.start_nutstore_upload(ids, auth, window, cx);
    }

    pub fn open_nutstore_auth(&mut self, pending: NutstorePending, cx: &mut Context<Self>) {
        let root = fdl_core::store::Store::root_dir();
        let auth = crate::nutstore::load_auth(&root);
        if let Some(f) = self.nutstore_email_field.as_ref() {
            f.update(cx, |f, cx| f.set_text(&auth.email, cx));
        }
        if let Some(f) = self.nutstore_password_field.as_ref() {
            f.update(cx, |f, cx| f.set_text(&auth.app_password, cx));
        }
        if let Some(f) = self.nutstore_root_field.as_ref() {
            let root_s = if auth.remote_root.trim().is_empty() {
                "Quiet.md".to_string()
            } else {
                auth.remote_root.clone()
            };
            f.update(cx, |f, cx| f.set_text(&root_s, cx));
        }
        self.nutstore_auth = Some(NutstoreAuthDlg {
            pending,
            error: None,
        });
        cx.notify();
    }

    pub fn confirm_nutstore_auth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.nutstore_auth.as_mut() else { return };
        let email = self
            .nutstore_email_field
            .as_ref()
            .map(|f| f.read(cx).text.clone())
            .unwrap_or_default()
            .trim()
            .to_string();
        let app_password = self
            .nutstore_password_field
            .as_ref()
            .map(|f| f.read(cx).text.clone())
            .unwrap_or_default()
            .trim()
            .to_string();
        let remote_root = self
            .nutstore_root_field
            .as_ref()
            .map(|f| f.read(cx).text.clone())
            .unwrap_or_else(|| "Quiet.md".into())
            .trim()
            .to_string();
        if email.is_empty() || app_password.is_empty() {
            state.error = Some(crate::settings::text("请填写坚果云邮箱和应用密码").to_string());
            cx.notify();
            return;
        }
        let auth = crate::nutstore::NutstoreAuth {
            email,
            app_password,
            remote_root: if remote_root.is_empty() {
                "Quiet.md".into()
            } else {
                remote_root
            },
        };
        let root = fdl_core::store::Store::root_dir();
        if let Err(e) = crate::nutstore::save_auth(&root, &auth) {
            state.error = Some(format!("保存失败：{e}"));
            cx.notify();
            return;
        }
        let pending = state.pending.clone();
        self.nutstore_auth = None;
        cx.notify();
        match pending {
            NutstorePending::Upload(ids) => self.start_nutstore_upload(ids, auth, window, cx),
            NutstorePending::Import => self.start_nutstore_list(auth, window, cx),
        }
    }

    fn start_nutstore_upload(
        &mut self,
        ids: Vec<String>,
        auth: crate::nutstore::NutstoreAuth,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let n = ids.len();
        self.show_toast_kind(
            ToastKind::Info,
            if n > 1 {
                crate::settings::text("正在上传到坚果云…")
            } else {
                crate::settings::text("正在上传到坚果云…")
            },
            cx,
        );
        let root = fdl_core::store::Store::root_dir();
        let titles: Vec<(String, String)> = ids
            .iter()
            .map(|id| {
                let title = self
                    .entries
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| display_title(&e.title))
                    .unwrap_or_else(|| id.clone());
                (id.clone(), title)
            })
            .collect();
        enum Ev {
            Progress(String),
            Done(Result<usize, String>),
        }
        let (tx, rx) = std::sync::mpsc::channel::<Ev>();
        let tx_p = tx.clone();
        cx.background_executor()
            .spawn(async move {
                let progress = move |msg: &str| {
                    let _ = tx_p.send(Ev::Progress(msg.to_string()));
                };
                let result = crate::nutstore::upload_docs(&ids, &titles, &auth, &root, Some(&progress));
                let _ = tx.send(Ev::Done(result.map_err(|e| format!("{e:#}"))));
            })
            .detach();
        cx.spawn_in(window, async move |this, cx| {
            let result = loop {
                let mut done = None;
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        Ev::Progress(msg) => {
                            let _ = this.update(cx, |w, cx| {
                                w.show_toast_kind(ToastKind::Info, &msg, cx);
                            });
                        }
                        Ev::Done(r) => done = Some(r),
                    }
                }
                if let Some(r) = done {
                    break r;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            };
            let _ = this.update(cx, |w, cx| {
                match result {
                    Ok(n) => {
                        w.batch = None;
                        w.show_toast(
                            &if n > 1 {
                                crate::localized_format!("已上传 {} 篇到坚果云", "Uploaded {} documents to Nutstore", n)
                            } else {
                                crate::settings::text("已上传到坚果云").to_string()
                            },
                            cx,
                        );
                    }
                    Err(e) => {
                        let msg = if e.contains("账号") || e.contains("应用密码") || e.contains("401") {
                            crate::settings::text("坚果云账号或应用密码不正确，请检查后重试").to_string()
                        } else {
                            e
                        };
                        w.show_toast_kind(ToastKind::Error, &msg, cx);
                    }
                }
            });
        })
        .detach();
    }

    pub(crate) fn render_nutstore_auth(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.nutstore_auth.as_ref()?;
        let err = state.error.clone();
        let ok_label = match &state.pending {
            NutstorePending::Upload(_) => crate::settings::text("保存并上传"),
            NutstorePending::Import => crate::settings::text("保存并继续"),
        };
        let email = self.nutstore_email_field.clone()?;
        let password = self.nutstore_password_field.clone()?;
        let root_f = self.nutstore_root_field.clone()?;
        let field_row = |label: &'static str, field: Entity<crate::input::TextField>| {
            div()
                .mb(px(12.))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(Theme::text_caption())
                        .mb(px(6.))
                        .child(crate::settings::text(label)),
                )
                .child(
                    div()
                        .h(px(36.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(Theme::line_divider())
                        .flex()
                        .items_center()
                        .child(field),
                )
        };
        let dialog = dialog("nutstore-auth")
            .relative()
            .w(px(480.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .pt(px(24.))
                    .pb(px(24.))
                    .pl(px(24.))
                    .pr(px(56.))
                    .border_b_1()
                    .border_color(Theme::line_divider())
                    .text_size(px(15.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::settings::text("坚果云账号")),
            )
            .child(
                close_button("nutstore-auth-close")
                    .absolute()
                    .top(px(27.))
                    .right(px(25.))
                    .on_click(cx.listener(|w, _, _, cx| {
                        w.nutstore_auth = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .pt(px(16.))
                    .px(px(24.))
                    .pb(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(Theme::text_caption())
                            .mb(px(12.))
                            .child(crate::settings::text(
                                "请填写坚果云邮箱和应用密码（不是登录密码）。应用密码在坚果云网页：账户信息 → 安全选项 → 第三方应用管理。",
                            )),
                    )
                    .child(field_row("邮箱", email))
                    .child(field_row("应用密码", password))
                    .child(field_row("云端文件夹", root_f))
                    .when_some(err, |d, e| {
                        d.child(
                            div()
                                .mt(px(4.))
                                .text_size(px(12.))
                                .text_color(Theme::text_caption())
                                .child(e),
                        )
                    }),
            )
            .child(
                div()
                    .px(px(24.))
                    .pb(px(24.))
                    .pt(px(16.))
                    .flex()
                    .justify_end()
                    .child(
                        secondary_button("nutstore-auth-cancel", crate::settings::text("取消"))
                            .min_w(px(80.))
                            .mr(px(12.))
                            .on_click(cx.listener(|w, _, _, cx| {
                                w.nutstore_auth = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        primary_button("nutstore-auth-ok", ok_label)
                            .min_w(px(80.))
                            .on_click(cx.listener(|w, _, window, cx| {
                                w.confirm_nutstore_auth(window, cx);
                            })),
                    ),
            );
        Some(mask(
            "nutstore-auth-mask",
            dialog,
            |w, cx| {
                w.nutstore_auth = None;
                cx.notify();
            },
            cx,
        ))
    }

    /// 从坚果云导入… (File menu). Lists remote zips after auth.
    pub fn open_import_nutstore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menus();
        let root = fdl_core::store::Store::root_dir();
        let auth = crate::nutstore::load_auth(&root);
        if !auth.is_ready() {
            self.open_nutstore_auth(NutstorePending::Import, cx);
            return;
        }
        self.start_nutstore_list(auth, window, cx);
    }

    fn start_nutstore_list(
        &mut self,
        auth: crate::nutstore::NutstoreAuth,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nutstore_import = Some(NutstoreImportDlg {
            items: vec![],
            busy: false,
            loading: true,
            error: None,
            status: crate::settings::text("正在列出坚果云文档包…").to_string(),
        });
        cx.notify();
        self.show_toast_kind(ToastKind::Info, crate::settings::text("正在连接坚果云…"), cx);
        enum Ev {
            Done(Result<Vec<String>, String>),
        }
        let (tx, rx) = std::sync::mpsc::channel::<Ev>();
        cx.background_executor()
            .spawn(async move {
                let result = crate::nutstore::list_remote_zips(&auth)
                    .map(|v| v.into_iter().map(|z| z.name).collect::<Vec<_>>())
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(Ev::Done(result));
            })
            .detach();
        cx.spawn_in(window, async move |this, cx| {
            let result = loop {
                if let Ok(Ev::Done(r)) = rx.try_recv() {
                    break r;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            };
            let _ = this.update(cx, |w, cx| {
                match result {
                    Ok(names) => {
                        if let Some(dlg) = w.nutstore_import.as_mut() {
                            dlg.loading = false;
                            dlg.status.clear();
                            dlg.error = None;
                            // Pre-select all for convenience.
                            dlg.items = names.into_iter().map(|n| (n, true)).collect();
                            if dlg.items.is_empty() {
                                dlg.error = Some(
                                    crate::settings::text("云端文件夹里还没有上传的文档包").to_string(),
                                );
                            }
                        }
                        cx.notify();
                    }
                    Err(e) => {
                        let msg = if e.contains("账号") || e.contains("应用密码") || e.contains("401") {
                            crate::settings::text("坚果云账号或应用密码不正确，请检查后重试").to_string()
                        } else {
                            e
                        };
                        if let Some(dlg) = w.nutstore_import.as_mut() {
                            dlg.loading = false;
                            dlg.error = Some(msg.clone());
                        } else {
                            w.show_toast_kind(ToastKind::Error, &msg, cx);
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn confirm_nutstore_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.nutstore_import.as_mut() else { return };
        if state.busy || state.loading {
            return;
        }
        let names: Vec<String> = state
            .items
            .iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n.clone())
            .collect();
        if names.is_empty() {
            state.error = Some(crate::settings::text("请先选择要导入的文档包").to_string());
            cx.notify();
            return;
        }
        state.busy = true;
        state.error = None;
        state.status = crate::settings::text("正在从坚果云导入…").to_string();
        cx.notify();
        self.show_toast_kind(ToastKind::Info, crate::settings::text("正在从坚果云导入…"), cx);
        let root = fdl_core::store::Store::root_dir();
        let auth = crate::nutstore::load_auth(&root);
        enum Ev {
            Progress(String),
            Done(Result<crate::nutstore::ImportReport, String>),
        }
        let (tx, rx) = std::sync::mpsc::channel::<Ev>();
        let tx_p = tx.clone();
        cx.background_executor()
            .spawn(async move {
                let progress = move |msg: &str| {
                    let _ = tx_p.send(Ev::Progress(msg.to_string()));
                };
                let result = crate::nutstore::import_remote_zips(&names, &auth, &root, Some(&progress));
                let _ = tx.send(Ev::Done(result.map_err(|e| format!("{e:#}"))));
            })
            .detach();
        cx.spawn_in(window, async move |this, cx| {
            let result = loop {
                let mut done = None;
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        Ev::Progress(msg) => {
                            let _ = this.update(cx, |w, cx| {
                                if let Some(s) = w.nutstore_import.as_mut() {
                                    s.status = msg.clone();
                                }
                                w.show_toast_kind(ToastKind::Info, &msg, cx);
                                cx.notify();
                            });
                        }
                        Ev::Done(r) => done = Some(r),
                    }
                }
                if let Some(r) = done {
                    break r;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            };
            let _ = this.update(cx, |w, cx| {
                match result {
                    Ok(rep) => {
                        w.nutstore_import = None;
                        w.entries = w.store.list();
                        w.sort();
                        let msg = if rep.skipped > 0 {
                            crate::localized_format!(
                                "已导入 {} 篇，跳过 {} 篇已存在",
                                "Imported {}, skipped {} existing",
                                rep.imported,
                                rep.skipped
                            )
                        } else if rep.imported == 0 {
                            crate::settings::text("没有可导入的新文档").to_string()
                        } else if rep.imported > 1 {
                            crate::localized_format!(
                                "已从坚果云导入 {} 篇",
                                "Imported {} documents from Nutstore",
                                rep.imported
                            )
                        } else {
                            crate::settings::text("已从坚果云导入").to_string()
                        };
                        w.show_toast(&msg, cx);
                        cx.notify();
                    }
                    Err(e) => {
                        let msg = if e.contains("账号") || e.contains("应用密码") || e.contains("401") {
                            crate::settings::text("坚果云账号或应用密码不正确，请检查后重试").to_string()
                        } else {
                            e
                        };
                        if let Some(s) = w.nutstore_import.as_mut() {
                            s.busy = false;
                            s.status.clear();
                            s.error = Some(msg.clone());
                        }
                        w.show_toast_kind(ToastKind::Error, &msg, cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub(crate) fn render_nutstore_import(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.nutstore_import.as_ref()?;
        let busy = state.busy;
        let loading = state.loading;
        let err = state.error.clone();
        let status = state.status.clone();
        let items = state.items.clone();
        let selected = items.iter().filter(|(_, on)| *on).count();
        let all_on = !items.is_empty() && items.iter().all(|(_, on)| *on);
        let list = div()
            .id("nutstore-import-list")
            .max_h(px(280.))
            .overflow_y_scroll()
            .border_1()
            .border_color(Theme::line_divider())
            .rounded(px(6.))
            .when(items.is_empty() && !loading, |d| {
                d.p(px(16.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(Theme::text_caption())
                            .child(crate::settings::text("没有可导入的文档包")),
                    )
            })
            .children(items.iter().enumerate().map(|(i, (name, on))| {
                let name_s = name.clone();
                let checked = *on;
                div()
                    .id(SharedString::from(format!("nz-{i}")))
                    .h(px(36.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(Theme::fill_hover()))
                    .on_click(cx.listener(move |w, _, _, cx| {
                        if let Some(dlg) = w.nutstore_import.as_mut() {
                            if let Some(item) = dlg.items.get_mut(i) {
                                item.1 = !item.1;
                            }
                        }
                        cx.notify();
                    }))
                    .child(crate::ui::checkbox(checked))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(px(13.))
                            .text_ellipsis()
                            .child(name_s),
                    )
            }));
        let dialog = dialog("nutstore-import")
            .relative()
            .w(px(520.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .pt(px(24.))
                    .pb(px(24.))
                    .pl(px(24.))
                    .pr(px(56.))
                    .border_b_1()
                    .border_color(Theme::line_divider())
                    .text_size(px(15.))
                    .line_height(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::settings::text("从坚果云导入")),
            )
            .child(
                close_button("nutstore-import-close")
                    .absolute()
                    .top(px(27.))
                    .right(px(25.))
                    .on_click(cx.listener(|w, _, _, cx| {
                        if w.nutstore_import.as_ref().is_some_and(|s| s.busy) {
                            return;
                        }
                        w.nutstore_import = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .pt(px(16.))
                    .px(px(24.))
                    .pb(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(Theme::text_caption())
                            .mb(px(10.))
                            .child(crate::settings::text("选择要导入的文档包（已存在的文档会跳过）")),
                    )
                    .when(!items.is_empty(), |d| {
                        d.child(
                            div()
                                .mb(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .id("nz-all")
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |w, _, _, cx| {
                                            if let Some(dlg) = w.nutstore_import.as_mut() {
                                                let to = !all_on;
                                                for item in &mut dlg.items {
                                                    item.1 = to;
                                                }
                                            }
                                            cx.notify();
                                        }))
                                        .child(crate::ui::checkbox(all_on))
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(Theme::text_caption())
                                                .child(crate::settings::text("全选")),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(Theme::text_caption())
                                        .child(crate::localized_format!("已选 {} 项", "Selected: {}", selected)),
                                ),
                        )
                    })
                    .child(list)
                    .when(loading || (busy && !status.is_empty()), |d| {
                        d.child(
                            div()
                                .mt(px(8.))
                                .text_size(px(12.))
                                .text_color(Theme::text_caption())
                                .child(if status.is_empty() {
                                    crate::settings::text("请稍候…").to_string()
                                } else {
                                    status
                                }),
                        )
                    })
                    .when_some(err, |d, e| {
                        d.child(
                            div()
                                .mt(px(8.))
                                .text_size(px(12.))
                                .text_color(Theme::text_caption())
                                .child(e),
                        )
                    }),
            )
            .child(
                div()
                    .px(px(24.))
                    .pb(px(24.))
                    .pt(px(16.))
                    .flex()
                    .justify_end()
                    .child(
                        secondary_button("nutstore-import-cancel", crate::settings::text("取消"))
                            .min_w(px(80.))
                            .mr(px(12.))
                            .on_click(cx.listener(|w, _, _, cx| {
                                if w.nutstore_import.as_ref().is_some_and(|s| s.busy) {
                                    return;
                                }
                                w.nutstore_import = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        primary_button(
                            "nutstore-import-ok",
                            if busy || loading {
                                crate::settings::text("导入中…")
                            } else {
                                crate::settings::text("导入")
                            },
                        )
                        .min_w(px(80.))
                        .on_click(cx.listener(|w, _, window, cx| {
                            w.confirm_nutstore_import(window, cx);
                        })),
                    ),
            );
        Some(mask(
            "nutstore-import-mask",
            dialog,
            |w, cx| {
                if w.nutstore_import.as_ref().is_some_and(|s| s.busy) {
                    return;
                }
                w.nutstore_import = None;
                cx.notify();
            },
            cx,
        ))
    }

}

#[cfg(test)]
mod tests {
    use super::{BlockKind, markdown_document, percent_decode};

    #[test]
    fn imports_markdown_file() {
        let dir = std::env::temp_dir().join(format!("fdl-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("周报.md");
        std::fs::write(&path, "\u{feff}# 周报\n\n第一段\n\n- a\n- b\n\n![图](https://e.com/a.png)\n\n## 小结\n").unwrap();
        let doc = markdown_document(&path).unwrap();
        assert_eq!(doc.title.text, "周报");
        let kinds: Vec<_> = doc.blocks.iter().map(|b| std::mem::discriminant(&b.kind)).collect();
        assert_eq!(kinds.len(), 5);
        assert_eq!(doc.blocks[0].text.text, "第一段");
        assert!(matches!(doc.blocks[1].kind, BlockKind::Bullet));
        assert!(matches!(&doc.blocks[3].kind, BlockKind::Image { src, .. } if src == "https://e.com/a.png"));
        assert!(matches!(doc.blocks[4].kind, BlockKind::Heading { level: 2, .. }));
        assert_eq!(percent_decode("a%20b%E5%9B%BE.png"), "a b图.png");
        let _ = std::fs::remove_dir_all(dir);
    }
}
