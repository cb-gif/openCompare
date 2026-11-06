#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod copy_ui;
mod directory_ui;
mod preview_ui;
mod search_ui;
mod session_ui;
#[cfg(test)]
mod tests;

use compare_app::{
    default_state_path, AppState, ComparisonKind, ComparisonWorker, Job, JobOutput,
    StoredComparison, TextSession,
};
use compare_core::{AlignedRow, CompareOptions, DocumentSnapshot, RowKind, Side};
use compare_fs::{
    DirectoryPhase, DirectoryReport, EntryKind, EntryStatus, LoadedDocument, TextEncoding,
};
use eframe::egui::{
    self, text::LayoutJob, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};
use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const ACCENT: Color32 = Color32::from_rgb(77, 211, 183);
const MUTED: Color32 = Color32::from_rgb(137, 151, 169);
const SURFACE: Color32 = Color32::from_rgb(24, 31, 42);
const ROW_HEIGHT: f32 = 25.0;

fn main() -> eframe::Result {
    let state_path = default_state_path();
    let (state, state_error) = match AppState::load(&state_path) {
        Ok(state) => (state, None),
        Err(error) => (AppState::default(), Some(error)),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(state.window_size)
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rust Compare",
        options,
        Box::new(move |cc| {
            Ok(Box::new(CompareApp::new(
                cc,
                state,
                state_path,
                state_error,
            )))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Text,
    Directory,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum BusyKind {
    CopyPlanning,
    Copy,
    Open,
    Compare,
    Directory,
    Save,
}
struct Busy {
    kind: BusyKind,
    started: Instant,
}
enum Action {
    Restore(StoredComparison),
    Open {
        left: PathBuf,
        right: PathBuf,
        directory: bool,
    },
    Demo,
    Exit,
}
enum TextAction {
    Page(bool),
    Find,
    Undo,
    Redo,
    Accept(Side),
    Edit(Side),
    Save(Side),
    Next(bool),
    Recompare,
}
struct Editor {
    side: Side,
    text: String,
}

struct CompareApp {
    preview_navigation: preview_ui::PreviewNavigation,
    directory_phase: Option<DirectoryPhase>,
    directory_index: std::collections::HashMap<PathBuf, usize>,
    directory_search_keys: Vec<String>,
    directory_counts: [usize; 3],
    copy: copy_ui::CopyUi,
    search: search_ui::SearchState,
    state: AppState,
    state_path: PathBuf,
    state_writable: bool,
    state_dirty_at: Option<Instant>,
    worker: ComparisonWorker,
    session: Option<TextSession>,
    directory: Option<DirectoryReport>,
    view: View,
    paths: [String; 2],
    encodings: [Option<TextEncoding>; 2],
    options: CompareOptions,
    busy: Option<Busy>,
    selected_hunk: usize,
    selected_entry: Option<usize>,
    directory_filter: String,
    hide_equal: bool,
    filtered_entries: Vec<usize>,
    jump_row: Option<usize>,
    horizontal_offset: f32,
    open_dialog: bool,
    open_directory: bool,
    editor: Option<Editor>,
    pending_action: Option<Action>,
    allow_close: bool,
    notice: String,
    error: Option<String>,
}

impl CompareApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        state: AppState,
        state_path: PathBuf,
        state_error: Option<String>,
    ) -> Self {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        Self::initialize(&cc.egui_ctx, state, state_path, state_error, &args)
    }

    fn initialize(
        ctx: &egui::Context,
        state: AppState,
        state_path: PathBuf,
        state_error: Option<String>,
        args: &[std::ffi::OsString],
    ) -> Self {
        configure_style(ctx);
        let mut app = Self {
            preview_navigation: preview_ui::PreviewNavigation::default(),
            directory_phase: None,
            directory_index: Default::default(),
            directory_search_keys: Vec::new(),
            directory_counts: [0; 3],
            copy: copy_ui::CopyUi::default(),
            search: search_ui::SearchState::default(),
            options: state.options,
            hide_equal: state.hide_equal,
            directory_filter: state.directory_filter.clone(),
            state_writable: state_error.is_none(),
            state,
            state_path,
            state_dirty_at: None,
            worker: ComparisonWorker::new(),
            session: None,
            directory: None,
            view: View::Text,
            paths: Default::default(),
            encodings: [None, None],
            busy: None,
            selected_hunk: 0,
            selected_entry: None,
            filtered_entries: Vec::new(),
            jump_row: None,
            horizontal_offset: 0.0,
            open_dialog: false,
            open_directory: false,
            editor: None,
            pending_action: None,
            allow_close: false,
            notice: "就绪 · 文件内容仅在本地处理".into(),
            error: None,
        };
        app.load_demo();
        if args.len() == 2 {
            let left = PathBuf::from(&args[0]);
            let right = PathBuf::from(&args[1]);
            let directory = left.is_dir() && right.is_dir();
            app.perform(
                Action::Open {
                    left,
                    right,
                    directory,
                },
                ctx,
            );
        } else if let Some(comparison) = app.state.last.clone() {
            app.restore_comparison(comparison, ctx);
        }
        if state_error.is_some() {
            app.error = state_error;
        }
        app
    }

    fn load_demo(&mut self) {
        self.session = Some(TextSession::new(
            LoadedDocument::from_text(include_str!("../../../fixtures/left/example.rs")),
            LoadedDocument::from_text(include_str!("../../../fixtures/right/example.rs")),
            self.options,
        ));
        self.view = View::Text;
        self.selected_hunk = 0;
        self.reset_preview_navigation();
        self.notice = "示例会话 · 打开两个文件开始比较，或拖入两个文件".into();
        self.search.refresh(self.session.as_ref());
    }

    fn submit(&mut self, job: Job, kind: BusyKind) {
        if let Job::CompareDirectories { left, right } = &job {
            self.directory = Some(DirectoryReport {
                left: std::path::absolute(left).unwrap_or_else(|_| left.clone()),
                right: std::path::absolute(right).unwrap_or_else(|_| right.clone()),
                entries: Vec::new(),
                complete: false,
            });
            self.directory_phase = Some(DirectoryPhase::Discovering);
            self.directory_index.clear();
            self.directory_search_keys.clear();
            self.filtered_entries.clear();
            self.directory_counts = [0; 3];
            self.copy.selection.clear();
            self.view = View::Directory;
        }
        self.worker.submit(job);
        self.busy = Some(Busy {
            kind,
            started: Instant::now(),
        });
        self.error = None;
    }

    fn recompare(&mut self) {
        self.search.refresh(None);
        if let Some(session) = self.session.as_ref().filter(|s| s.is_read_only()) {
            if let (Some(left), Some(right)) = (&session.left.path, &session.right.path) {
                let job = Job::OpenFiles {
                    left: left.clone(),
                    right: right.clone(),
                    left_encoding: Some(session.left.format.encoding),
                    right_encoding: Some(session.right.format.encoding),
                    options: self.options,
                };
                self.submit(job, BusyKind::Open);
                return;
            }
        }
        if let Some(session) = &mut self.session {
            session.diff = None;
            let job = session.comparison_job(self.options);
            self.submit(job, BusyKind::Compare);
        }
    }

    fn poll(&mut self) {
        let updates = self.worker.poll_directory_updates();
        if !updates.is_empty() {
            for batch in updates {
                self.apply_directory_batch(batch);
            }
            self.refresh_directory_filter();
        }
        let Some(event) = self.worker.poll() else {
            return;
        };
        let previous_kind = self.busy.as_ref().map(|busy| busy.kind);
        let elapsed = self
            .busy
            .take()
            .map_or(Duration::ZERO, |b| b.started.elapsed());
        match event.result {
            Err(error) => {
                if previous_kind == Some(BusyKind::Directory) {
                    self.directory_phase = Some(DirectoryPhase::Failed);
                }
                self.preview_navigation.pending = None;
                self.error = Some(error);
                self.notice = "操作失败，原会话仍保留".into();
            }
            Ok(JobOutput::Opened(session)) => {
                self.session = Some(*session);
                self.view = View::Text;
                self.selected_hunk = 0;
                self.jump_row = Some(0);
                self.reset_preview_navigation();
                self.notice = format!("比较完成 · {:.0} ms", elapsed.as_secs_f64() * 1000.0);
                self.remember_current();
                self.search.refresh(self.session.as_ref());
            }
            Ok(JobOutput::Compared(diff)) => {
                if let Some(session) = &mut self.session {
                    let count = diff.hunks.len();
                    if session.install_diff(diff) {
                        self.selected_hunk = self.selected_hunk.min(count.saturating_sub(1));
                        self.notice = format!(
                            "比较完成 · {count} 个差异块 · {:.0} ms",
                            elapsed.as_secs_f64() * 1000.0
                        );
                    }
                }
                self.remember_current();
                self.search.refresh(self.session.as_ref());
            }
            Ok(JobOutput::Directory(report)) => {
                self.notice = format!(
                    "扫描完成 · {} 项 · {} 项差异 · {} 个错误",
                    report.entries.len(),
                    report.different_count(),
                    report.error_count()
                );
                self.directory = Some(report);
                self.directory_phase = Some(DirectoryPhase::Complete);
                self.directory_index.clear();
                self.directory_search_keys.clear();
                self.selected_entry = None;
                self.view = View::Directory;
                self.refresh_directory_filter();
                self.remember_current();
            }
            Ok(JobOutput::Saved { side, document }) => {
                if let Some(session) = &mut self.session {
                    *session.document_mut(side) = document;
                }
                self.notice = format!("{}文件已保存", side_label(side));
            }
            Ok(JobOutput::Searched(_)) => {}
            Ok(JobOutput::PreviewPage(session)) => {
                self.session = Some(*session);
                if let Some((index, cursors)) = self.preview_navigation.pending.take() {
                    if index >= self.preview_navigation.history.len() {
                        self.preview_navigation.history.push(cursors);
                    }
                    self.preview_navigation.current = index;
                }
                self.selected_hunk = 0;
                self.jump_row = Some(0);
                self.horizontal_offset = 0.0;
                self.search.refresh(self.session.as_ref());
                self.notice = "只读预览已翻页；差异与查找仅覆盖当前页".into();
            }
            Ok(JobOutput::CopyPrepared(plan)) => {
                self.copy.preview = Some(plan);
            }
            Ok(JobOutput::Copied(report)) => self.finish_copy(report),
        }
    }

    fn guarded(&mut self, action: Action, ctx: &egui::Context) {
        if self.session.as_ref().is_some_and(TextSession::is_dirty) {
            self.pending_action = Some(action);
        } else {
            self.perform(action, ctx);
        }
    }

    fn perform(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Restore(comparison) => self.restore_comparison(comparison, ctx),
            Action::Exit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::Demo => {
                self.worker.cancel();
                self.busy = None;
                self.load_demo();
                self.remember_current();
            }
            Action::Open {
                left,
                right,
                directory,
            } => {
                self.search.refresh(None);
                self.paths = [
                    left.to_string_lossy().into_owned(),
                    right.to_string_lossy().into_owned(),
                ];
                if directory {
                    self.submit(Job::CompareDirectories { left, right }, BusyKind::Directory);
                } else {
                    self.submit(
                        Job::OpenFiles {
                            left,
                            right,
                            left_encoding: self.encodings[0],
                            right_encoding: self.encodings[1],
                            options: self.options,
                        },
                        BusyKind::Open,
                    );
                }
            }
        }
    }

    fn handle_text_action(&mut self, action: TextAction) {
        match action {
            TextAction::Page(forward) => self.request_preview_page(forward),
            TextAction::Find => {
                self.search.visible = true;
                self.search.focus_requested = true;
                self.search.refresh(self.session.as_ref());
            }
            TextAction::Next(forward) => {
                if let Some(diff) = self.session.as_ref().and_then(|s| s.diff.as_ref()) {
                    if !diff.hunks.is_empty() {
                        let count = diff.hunks.len();
                        self.selected_hunk = if forward {
                            (self.selected_hunk + 1) % count
                        } else {
                            (self.selected_hunk + count - 1) % count
                        };
                        self.jump_row = Some(diff.hunks[self.selected_hunk].first_row);
                    }
                }
            }
            TextAction::Edit(side) => {
                if let Some(session) = &self.session {
                    if session.is_read_only() {
                        return;
                    }
                    self.editor = Some(Editor {
                        side,
                        text: session.document(side).document.text().to_owned(),
                    });
                }
            }
            TextAction::Save(side) => {
                if let Some(session) = &self.session {
                    if session.is_read_only() {
                        return;
                    }
                    self.submit(
                        Job::Save {
                            side,
                            document: session.document(side).clone(),
                        },
                        BusyKind::Save,
                    );
                }
            }
            TextAction::Recompare => self.recompare(),
            other => {
                if let Some(session) = &mut self.session {
                    if session.is_read_only() {
                        return;
                    }
                    match other {
                        TextAction::Undo => session.undo(),
                        TextAction::Redo => session.redo(),
                        TextAction::Accept(side) => {
                            if let Err(e) = session.accept(self.selected_hunk, side) {
                                self.error = Some(e.to_string());
                            }
                        }
                        _ => {}
                    }
                    self.recompare();
                }
            }
        }
    }

    fn header(&mut self, ctx: &egui::Context, enabled: bool) {
        egui::TopBottomPanel::top("header")
            .exact_height(68.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(17, 23, 32))
                    .inner_margin(18.0),
            )
            .show(ctx, |ui| {
                ui.add_enabled_ui(enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("RC").size(23.0).strong().color(ACCENT));
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Rust Compare").size(20.0).strong());
                            ui.label(RichText::new("文件与目录比较工作台").small().color(MUTED));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("打开文件夹").clicked() {
                                self.open_directory = true;
                                self.open_dialog = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("打开文件")
                                            .color(Color32::from_rgb(15, 35, 35)),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                self.open_directory = false;
                                self.open_dialog = true;
                            }
                            ui.label(RichText::new("本地运行  /  v0.1 原型").small().color(MUTED));
                        });
                    });
                });
            });
    }

    fn sidebar(&mut self, ctx: &egui::Context, enabled: bool) {
        egui::SidePanel::left("navigation").exact_width(172.0).resizable(false)
            .frame(egui::Frame::new().fill(Color32::from_rgb(20, 27, 37)).inner_margin(16.0)).show(ctx, |ui| {
                ui.add_enabled_ui(enabled, |ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("工作空间").small().color(MUTED));
                    ui.add_space(14.0);
                    if ui.selectable_label(self.view == View::Text, "文本比较").clicked() { self.view = View::Text; self.remember_current(); }
                    ui.add_space(6.0);
                    if ui.selectable_label(self.view == View::Directory, "目录比较").clicked() { self.view = View::Directory; self.remember_current(); }
                    ui.add_space(22.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.label(RichText::new("快速开始").small().color(MUTED));
                    ui.add_space(8.0);
                    if ui.button("打开示例会话").clicked() { self.guarded(Action::Demo, ctx); }
                    self.recent_sessions(ui);
                    ui.add_space(20.0);
                    ui.label(RichText::new("Ctrl+O   打开\nCtrl+F   查找\nF3 / Shift+F3   匹配导航\nF7 / Shift+F7   差异导航\n\n可拖入两个文件\n或两个文件夹").size(12.0).color(MUTED));
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        ui.label(RichText::new("RUST ENGINE\n离线 · 精确 · 可撤销").size(11.0).color(MUTED));
                    });
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(34.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    if let Some(busy) = &self.busy {
                        ui.spinner();
                        let text = match busy.kind {
                            BusyKind::CopyPlanning => "正在准备复制预览",
                            BusyKind::Copy => "正在复制文件",
                            BusyKind::Open => "正在读取文件",
                            BusyKind::Compare => "正在计算差异",
                            BusyKind::Directory => "正在扫描与比较目录",
                            BusyKind::Save => "正在保存",
                        };
                        ui.label(format!(
                            "{text} · {} 项 · {:.1}s",
                            self.worker.progress.load(Ordering::Relaxed),
                            busy.started.elapsed().as_secs_f32()
                        ));
                        if busy.kind != BusyKind::Save && ui.small_button("取消").clicked() {
                            self.cancel_current_job();
                        }
                    } else {
                        ui.label(RichText::new(&self.notice).small().color(MUTED));
                    }
                });
            });
    }

    fn text_view(&mut self, ui: &mut egui::Ui) {
        let Some(session) = &self.session else {
            ui.label("打开两个文件开始比较");
            return;
        };
        let mut action = None;
        let ready = self.busy.is_none();
        let editable = ready && !session.is_read_only();
        let count = session.diff.as_ref().map_or(0, |d| d.hunks.len());
        ui.horizontal(|ui| {
            ui.label(RichText::new("文本比较").size(21.0).strong());
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!(
                    "{}{count} 个差异块",
                    if session.is_read_only() {
                        "当前页 "
                    } else {
                        ""
                    }
                ))
                .color(if count == 0 {
                    ACCENT
                } else {
                    Color32::from_rgb(245, 190, 115)
                }),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("查找").clicked() {
                    action = Some(TextAction::Find);
                }
                if ui
                    .add_enabled(ready, egui::Button::new("重新比较"))
                    .clicked()
                {
                    action = Some(TextAction::Recompare);
                }
                if ui
                    .add_enabled(ready && session.can_redo(), egui::Button::new("重做"))
                    .clicked()
                {
                    action = Some(TextAction::Redo);
                }
                if ui
                    .add_enabled(ready && session.can_undo(), egui::Button::new("撤销"))
                    .clicked()
                {
                    action = Some(TextAction::Undo);
                }
            });
        });
        ui.add_space(12.0);
        if let Some(page_action) = self.preview_toolbar(ui) {
            action = Some(page_action);
        }
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("比较规则").small().color(MUTED));
            ui.add_enabled_ui(editable, |ui| {
                changed |= ui
                    .checkbox(&mut self.options.ignore_trailing_whitespace, "忽略行尾空白")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.options.ignore_case, "忽略大小写")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.options.ignore_line_endings, "忽略换行符")
                    .changed();
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready && count > 0, egui::Button::new("上一处"))
                .clicked()
            {
                action = Some(TextAction::Next(false));
            }
            if ui
                .add_enabled(ready && count > 0, egui::Button::new("下一处"))
                .clicked()
            {
                action = Some(TextAction::Next(true));
            }
            ui.label(
                RichText::new(if count > 0 {
                    format!("{} / {count}", self.selected_hunk + 1)
                } else {
                    "—".into()
                })
                .small()
                .color(MUTED),
            );
            ui.add_space(16.0);
            if ui
                .add_enabled(editable && count > 0, egui::Button::new("取左 → 右"))
                .clicked()
            {
                action = Some(TextAction::Accept(Side::Left));
            }
            if ui
                .add_enabled(editable && count > 0, egui::Button::new("左 ← 取右"))
                .clicked()
            {
                action = Some(TextAction::Accept(Side::Right));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("点击差异行选择整块").small().color(MUTED));
            });
        });
        ui.add_space(12.0);
        ui.columns(2, |columns| {
            for (column, side) in columns.iter_mut().zip([Side::Left, Side::Right]) {
                let document = session.document(side);
                egui::Frame::new()
                    .fill(SURFACE)
                    .inner_margin(10.0)
                    .show(column, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(side_label(side)).small().color(ACCENT));
                            let name = document
                                .path
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .map_or_else(
                                    || "example.rs · 示例".into(),
                                    |n| n.to_string_lossy().into_owned(),
                                );
                            ui.label(
                                RichText::new(format!(
                                    "{name}{}",
                                    if document.is_dirty() { " *" } else { "" }
                                ))
                                .strong(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            editable
                                                && document.is_dirty()
                                                && document.path.is_some(),
                                            egui::Button::new("保存"),
                                        )
                                        .clicked()
                                    {
                                        action = Some(TextAction::Save(side));
                                    }
                                    if ui
                                        .add_enabled(
                                            editable
                                                && document.document.text().len() <= 512 * 1024,
                                            egui::Button::new("编辑"),
                                        )
                                        .on_hover_text(
                                            "原型的自由编辑限 512 KiB；较大文件仍可按差异块合并",
                                        )
                                        .clicked()
                                    {
                                        action = Some(TextAction::Edit(side));
                                    }
                                },
                            );
                        });
                        ui.label(
                            RichText::new(format!(
                                "{}  ·  {}  ·  {} 行",
                                document.format.encoding.label(),
                                document.line_ending_label(),
                                document.document.line_count()
                            ))
                            .small()
                            .color(MUTED),
                        );
                        if let Some(path) = &document.path {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(path.to_string_lossy()).small().color(MUTED),
                                )
                                .truncate(),
                            );
                        }
                        if let Some(preview) = &document.preview {
                            ui.label(
                                RichText::new(format!(
                                    "第 {} 行{}起 · 字节 {}–{} / {}",
                                    preview.cursor.line + 1,
                                    if preview.cursor.continuation {
                                        "（续）"
                                    } else {
                                        ""
                                    },
                                    preview.cursor.byte_offset,
                                    preview.end.byte_offset,
                                    preview.file_bytes
                                ))
                                .small()
                                .color(MUTED),
                            );
                        }
                    });
            }
        });
        ui.add_space(6.0);
        if changed {
            self.recompare();
        } else if let Some(action) = action {
            self.handle_text_action(action);
        }
        self.search_bar(ui);
        let session = self.session.as_ref().expect("session exists");
        let Some(diff) = &session.diff else {
            ui.centered_and_justified(|ui| {
                ui.label("正在准备对齐视图；可在状态栏取消后重新比较");
            });
            return;
        };
        if diff.coarse_alignment {
            ui.label(
                RichText::new("复杂输入已采用较粗的差异对齐，内容仍完整保留")
                    .small()
                    .color(MUTED),
            );
        }
        if diff.rows.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(if session.is_read_only() {
                    "两侧当前页均无文本"
                } else {
                    "两份文件均为空"
                });
            });
            return;
        }
        let height = (ui.available_height() - 30.0).max(60.0);
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("text-diff")
            .auto_shrink([false, false])
            .max_height(height);
        if let Some(row) = self.jump_row.take() {
            scroll = scroll.vertical_scroll_offset(row as f32 * ROW_HEIGHT);
        }
        let selected = &mut self.selected_hunk;
        let offset = self.horizontal_offset;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            scroll.show_rows(ui, ROW_HEIGHT, diff.rows.len(), |ui, range| {
                for row in &diff.rows[range] {
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), ROW_HEIGHT),
                        Sense::click(),
                    );
                    if response.clicked() {
                        if let Some(hunk) = row.hunk {
                            *selected = hunk;
                        }
                    }
                    paint_diff_row(
                        ui,
                        rect,
                        row,
                        &RowPaint {
                            left: &session.left.document,
                            right: &session.right.document,
                            selected: row.hunk == Some(*selected),
                            offset,
                            search: &self.search,
                            line_offsets: [
                                session.left.preview.as_ref().map_or(0, |p| p.cursor.line),
                                session.right.preview.as_ref().map_or(0, |p| p.cursor.line),
                            ],
                        },
                    );
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("水平偏移").small().color(MUTED));
            ui.add(egui::Slider::new(&mut self.horizontal_offset, 0.0..=2000.0).show_value(false));
            ui.label(
                RichText::new("两侧同步滚动 · 长行作局部预览，查找可定位全文，原文完整保留")
                    .size(11.0)
                    .color(MUTED),
            );
        });
    }

    fn refresh_directory_filter(&mut self) {
        self.copy.selection.clear();
        self.filtered_entries.clear();
        self.directory_counts = [0; 3];
        let filter = self.directory_filter.to_lowercase();
        if let Some(report) = &self.directory {
            self.directory_search_keys.extend(
                report.entries[self.directory_search_keys.len()..]
                    .iter()
                    .map(|entry| entry.relative_path.to_string_lossy().to_lowercase()),
            );
            for (index, entry) in report.entries.iter().enumerate() {
                match entry.status {
                    EntryStatus::Pending => self.directory_counts[1] += 1,
                    EntryStatus::Equal | EntryStatus::Directory => {}
                    EntryStatus::Error => {
                        self.directory_counts[0] += 1;
                        self.directory_counts[2] += 1;
                    }
                    _ => self.directory_counts[0] += 1,
                }
                if (!self.hide_equal
                    || !matches!(entry.status, EntryStatus::Equal | EntryStatus::Directory))
                    && (filter.is_empty() || self.directory_search_keys[index].contains(&filter))
                {
                    self.filtered_entries.push(index);
                }
            }
        }
    }

    fn directory_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("目录比较").size(21.0).strong());
            ui.add_space(12.0);
            ui.label(
                RichText::new("按相对路径配对 · 精确比较文件内容")
                    .small()
                    .color(MUTED),
            );
        });
        ui.add_space(12.0);
        let changed = ui
            .horizontal(|ui| {
                let a = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.directory_filter)
                            .hint_text("筛选相对路径…")
                            .desired_width(280.0),
                    )
                    .changed();
                let b = ui
                    .checkbox(&mut self.hide_equal, "隐藏相同项和目录容器")
                    .changed();
                a || b
            })
            .inner;
        if changed {
            self.refresh_directory_filter();
            self.mark_state_dirty();
        }
        self.copy_toolbar(ui);
        self.directory_progress_label(ui);
        let Some(report) = &self.directory else {
            ui.add_space(40.0);
            ui.label("点击右上角“打开文件夹”，选择两个目录开始比较。");
            ui.label(RichText::new("支持二进制文件精确比较；默认不跟随符号链接。").color(MUTED));
            return;
        };
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!("左：{}", report.left.display()))
                .small()
                .color(MUTED),
        );
        ui.label(
            RichText::new(format!("右：{}", report.right.display()))
                .small()
                .color(MUTED),
        );
        ui.add_space(10.0);
        ui.label(format!(
            "{} 项 · {} 项已确认差异 · {} 个待比较 · {} 个读取错误 · 当前显示 {} 项",
            report.entries.len(),
            self.directory_counts[0],
            self.directory_counts[1],
            self.directory_counts[2],
            self.filtered_entries.len()
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("相对路径").color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("双击两侧均存在的文本文件打开比较")
                        .small()
                        .color(MUTED),
                );
            });
        });
        ui.separator();
        let mut open = None;
        let selected = &mut self.selected_entry;
        let selection = &mut self.copy.selection;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical()
                .id_salt("directory")
                .auto_shrink([false, false])
                .show_rows(ui, 30.0, self.filtered_entries.len(), |ui, range| {
                    for filtered in range {
                        let index = self.filtered_entries[filtered];
                        let entry = &report.entries[index];
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 30.0),
                            Sense::click(),
                        );
                        if response.clicked() && report.complete {
                            *selected = Some(index);
                            if ui.input(|i| i.modifiers.command) {
                                if !selection.remove(&index) {
                                    selection.insert(index);
                                }
                            } else {
                                selection.clear();
                                selection.insert(index);
                            }
                        }
                        if response.double_clicked()
                            && self.busy.is_none()
                            && report.complete
                            && entry.left_kind == Some(EntryKind::File)
                            && entry.right_kind == Some(EntryKind::File)
                        {
                            open = Some((
                                report.left.join(&entry.relative_path),
                                report.right.join(&entry.relative_path),
                            ));
                        }
                        if selection.contains(&index) {
                            ui.painter()
                                .rect_filled(rect, 2.0, Color32::from_rgb(34, 62, 65));
                        }
                        let painter = ui.painter().with_clip_rect(rect);
                        painter.text(
                            Pos2::new(rect.left() + 8.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            entry.relative_path.to_string_lossy(),
                            FontId::proportional(13.0),
                            Color32::from_rgb(213, 222, 231),
                        );
                        let (label, color) = status_style(entry.status);
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(rect.right() - 166.0, rect.top()),
                                Vec2::new(166.0, rect.height()),
                            ),
                            0.0,
                            SURFACE,
                        );
                        painter.text(
                            Pos2::new(rect.right() - 150.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            label,
                            FontId::proportional(12.0),
                            color,
                        );
                        if let Some(error) = &entry.error {
                            response.on_hover_text(error);
                        }
                    }
                });
        });
        if let Some((left, right)) = open {
            self.guarded(
                Action::Open {
                    left,
                    right,
                    directory: false,
                },
                ui.ctx(),
            );
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.open_dialog {
            let mut visible = true;
            let mut open = false;
            egui::Window::new(if self.open_directory {
                "打开目录比较"
            } else {
                "打开文本比较"
            })
            .collapsible(false)
            .resizable(false)
            .default_width(620.0)
            .open(&mut visible)
            .show(ctx, |ui| {
                for index in 0..2 {
                    ui.add_space(8.0);
                    ui.label(if index == 0 {
                        "左侧路径"
                    } else {
                        "右侧路径"
                    });
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.paths[index]).desired_width(500.0),
                        );
                        if ui.button("浏览…").clicked() {
                            let dialog = rfd::FileDialog::new();
                            let path = if self.open_directory {
                                dialog.pick_folder()
                            } else {
                                dialog.pick_file()
                            };
                            if let Some(path) = path {
                                self.paths[index] = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                    if !self.open_directory {
                        egui::ComboBox::from_id_salt(("encoding", index))
                            .selected_text(
                                self.encodings[index]
                                    .map_or("自动（BOM / UTF-8）", TextEncoding::label),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.encodings[index],
                                    None,
                                    "自动（BOM / UTF-8）",
                                );
                                for encoding in [
                                    TextEncoding::Utf8,
                                    TextEncoding::Utf16Le,
                                    TextEncoding::Utf16Be,
                                    TextEncoding::Gbk,
                                ] {
                                    ui.selectable_value(
                                        &mut self.encodings[index],
                                        Some(encoding),
                                        encoding.label(),
                                    );
                                }
                            });
                    }
                }
                ui.add_space(16.0);
                if ui
                    .add_enabled(
                        self.paths.iter().all(|p| !p.trim().is_empty()),
                        egui::Button::new("开始比较").fill(Color32::from_rgb(35, 92, 81)),
                    )
                    .clicked()
                {
                    open = true;
                }
            });
            self.open_dialog = visible && !open;
            if open {
                self.guarded(
                    Action::Open {
                        left: PathBuf::from(&self.paths[0]),
                        right: PathBuf::from(&self.paths[1]),
                        directory: self.open_directory,
                    },
                    ctx,
                );
            }
        }
        if let Some(mut editor) = self.editor.take() {
            let mut apply = false;
            let mut cancel = false;
            egui::Window::new(format!("编辑{}文件", side_label(editor.side)))
                .collapsible(false)
                .default_size([840.0, 520.0])
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("应用后更新比较视图；点击主界面的保存按钮才会写入磁盘。")
                            .small()
                            .color(MUTED),
                    );
                    egui::ScrollArea::both().max_height(430.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut editor.text)
                                .font(egui::TextStyle::Monospace)
                                .code_editor()
                                .desired_width(780.0)
                                .desired_rows(24),
                        );
                    });
                    ui.horizontal(|ui| {
                        if ui.button("应用修改").clicked() {
                            apply = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
            if apply {
                if let Some(session) = &mut self.session {
                    session.edit(editor.side, editor.text);
                }
                self.recompare();
            } else if !cancel {
                self.editor = Some(editor);
            }
        }
        if self.pending_action.is_some() {
            let mut discard = false;
            let mut cancel = false;
            egui::Window::new("存在未保存的修改")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("继续会离开当前文本会话。可以返回编辑区先保存文件。");
                    ui.horizontal(|ui| {
                        if ui.button("返回继续编辑").clicked() {
                            cancel = true;
                        }
                        if ui.button("放弃修改并继续").clicked() {
                            discard = true;
                        }
                    });
                });
            if cancel {
                self.pending_action = None;
            }
            if discard {
                if let Some(action) = self.pending_action.take() {
                    self.perform(action, ctx);
                }
            }
        }
        if let Some(error) = self.error.clone() {
            let mut visible = true;
            egui::Window::new("操作未完成")
                .collapsible(false)
                .default_width(520.0)
                .open(&mut visible)
                .show(ctx, |ui| {
                    ui.label(RichText::new(error).color(Color32::from_rgb(245, 157, 151)));
                });
            if !visible {
                self.error = None;
            }
        }
    }
}

impl eframe::App for CompareApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.search.poll(self.session.as_ref());
        if let Some(row) = self.search.take_jump() {
            self.jump_row = Some(row);
            self.horizontal_offset = 0.0;
        }
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            let size = [rect.width().max(900.0), rect.height().max(600.0)];
            if size != self.state.window_size {
                self.state.window_size = size;
                self.mark_state_dirty();
            }
        }
        if self
            .state_dirty_at
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(400))
        {
            self.flush_state();
        }
        if self.busy.is_some() || self.search.busy || self.state_dirty_at.is_some() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self
                .busy
                .as_ref()
                .is_some_and(|b| matches!(b.kind, BusyKind::Save | BusyKind::Copy))
                || self.editor.is_some()
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.notice = "请先等待文件操作结束或关闭编辑窗口".into();
            } else if self.session.as_ref().is_some_and(TextSession::is_dirty) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.pending_action = Some(Action::Exit);
            }
        }
        let blocked = self.busy.is_some()
            || self.editor.is_some()
            || self.pending_action.is_some()
            || self.open_dialog
            || self.copy.preview.is_some()
            || self.copy.report.is_some();
        if !blocked {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
                self.open_dialog = true;
            }
            if self.view == View::Text {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
                    self.handle_text_action(TextAction::Find);
                }
                if self.search.visible
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F3))
                {
                    self.search.step(true);
                }
                if self.search.visible
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3))
                {
                    self.search.step(false);
                }
                if self.search.visible
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                {
                    self.search.visible = false;
                    self.search.refresh(None);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F7)) {
                    self.handle_text_action(TextAction::Next(true));
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F7)) {
                    self.handle_text_action(TextAction::Next(false));
                }
                if !ctx.wants_keyboard_input()
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z))
                {
                    self.handle_text_action(TextAction::Undo);
                }
            }
            let paths: Vec<_> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect()
            });
            if paths.len() == 2 {
                let directory = paths[0].is_dir() && paths[1].is_dir();
                self.guarded(
                    Action::Open {
                        left: paths[0].clone(),
                        right: paths[1].clone(),
                        directory,
                    },
                    ctx,
                );
            }
        }
        self.header(ctx, !blocked);
        self.footer(ctx);
        self.sidebar(ctx, !blocked);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(27, 34, 45))
                    .inner_margin(18.0),
            )
            .show(ctx, |ui| {
                // Job controls in the footer remain available while background work runs.
                ui.add_enabled_ui(
                    self.editor.is_none()
                        && self.pending_action.is_none()
                        && !self.open_dialog
                        && self.copy.preview.is_none()
                        && self.copy.report.is_none()
                        && (self.busy.is_none()
                            || (self.view == View::Directory
                                && self
                                    .busy
                                    .as_ref()
                                    .is_some_and(|b| b.kind == BusyKind::Directory))),
                    |ui| match self.view {
                        View::Text => self.text_view(ui),
                        View::Directory => self.directory_view(ui),
                    },
                );
            });
        self.dialogs(ctx);
        self.copy_dialogs(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_state();
    }
}

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Left => "左侧",
        Side::Right => "右侧",
    }
}

fn status_style(status: EntryStatus) -> (&'static str, Color32) {
    match status {
        EntryStatus::Pending => ("待比较", MUTED),
        EntryStatus::Equal => ("相同", MUTED),
        EntryStatus::Directory => ("目录", MUTED),
        EntryStatus::Different => ("内容不同", Color32::from_rgb(245, 190, 115)),
        EntryStatus::LeftOnly => ("仅左侧", Color32::from_rgb(242, 155, 151)),
        EntryStatus::RightOnly => ("仅右侧", ACCENT),
        EntryStatus::TypeConflict => ("文件类型冲突", Color32::from_rgb(232, 166, 245)),
        EntryStatus::Error => ("读取失败", Color32::from_rgb(255, 118, 118)),
    }
}

struct RowPaint<'a> {
    left: &'a DocumentSnapshot,
    right: &'a DocumentSnapshot,
    selected: bool,
    offset: f32,
    line_offsets: [u64; 2],
    search: &'a search_ui::SearchState,
}

fn paint_diff_row(ui: &egui::Ui, rect: Rect, row: &AlignedRow, paint: &RowPaint<'_>) {
    let half = (rect.width() - 24.0) / 2.0;
    for (side, index, document, spans) in [
        (Side::Left, row.left, paint.left, &row.left_spans),
        (Side::Right, row.right, paint.right, &row.right_spans),
    ] {
        let x = if side == Side::Left {
            rect.left()
        } else {
            rect.left() + half + 24.0
        };
        let cell = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(half, rect.height()));
        let changed = row.kind != RowKind::Equal;
        let background = if index.is_none() {
            Color32::from_rgb(29, 35, 44)
        } else if changed && side == Side::Left {
            Color32::from_rgb(61, 37, 46)
        } else if changed {
            Color32::from_rgb(28, 56, 50)
        } else {
            SURFACE
        };
        ui.painter().rect_filled(cell, 0.0, background);
        if paint.selected {
            ui.painter().line_segment(
                [cell.left_top(), cell.left_bottom()],
                Stroke::new(2.0_f32, ACCENT),
            );
        }
        let Some(index) = index else { continue };
        let gutter = Rect::from_min_size(cell.min, Vec2::new(48.0, cell.height()));
        ui.painter().with_clip_rect(gutter).text(
            Pos2::new(gutter.right() - 9.0, cell.center().y),
            egui::Align2::RIGHT_CENTER,
            index as u64 + paint.line_offsets[usize::from(side == Side::Right)] + 1,
            FontId::monospace(12.0),
            MUTED,
        );
        let content_rect = Rect::from_min_max(Pos2::new(gutter.right(), cell.top()), cell.max);
        let line = document.line_content(index);
        let active = paint.search.active(side, index);
        let search_spans = paint.search.spans(side, index);
        let mut start = active
            .filter(|range| range.end > 4000)
            .map_or(0, |range| range.start.saturating_sub(32));
        while !line.is_char_boundary(start) {
            start += 1;
        }
        let mut end = line.len().min(start + 4000);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        let mut job = LayoutJob::default();
        let normal = egui::TextFormat {
            font_id: FontId::monospace(13.0),
            color: Color32::from_rgb(218, 226, 235),
            ..Default::default()
        };
        let highlight = egui::TextFormat {
            background: if side == Side::Left {
                Color32::from_rgb(119, 57, 68)
            } else {
                Color32::from_rgb(35, 105, 82)
            },
            ..normal.clone()
        };
        if start > 0 {
            job.append("… ", 0.0, normal.clone());
        }
        let mut boundaries = std::collections::BTreeSet::from([start, end]);
        for range in spans.iter().chain(search_spans.iter()) {
            if range.end > start && range.start < end {
                boundaries.insert(range.start.clamp(start, end));
                boundaries.insert(range.end.clamp(start, end));
            }
        }
        let boundaries: Vec<_> = boundaries.into_iter().collect();
        for window in boundaries.windows(2) {
            let range = window[0]..window[1];
            let mut format = if spans.iter().any(|span| span.contains(&range.start)) {
                highlight.clone()
            } else {
                normal.clone()
            };
            if search_spans.iter().any(|span| span.contains(&range.start)) {
                format.background = Color32::from_rgb(130, 99, 40);
            }
            if active.is_some_and(|span| span.contains(&range.start)) {
                format.background = Color32::from_rgb(255, 214, 119);
                format.color = Color32::from_rgb(30, 30, 30);
            }
            job.append(&line[range], 0.0, format);
        }
        if end < line.len() {
            job.append(
                " … [长行预览]",
                0.0,
                egui::TextFormat {
                    color: MUTED,
                    ..normal.clone()
                },
            );
        }
        if changed {
            let ending = compare_core::split_ending(document.line(index)).1;
            let marker = match ending {
                "\r\n" => "  [CRLF]",
                "\n" => "  [LF]",
                _ => "  [无末尾换行]",
            };
            job.append(
                marker,
                0.0,
                egui::TextFormat {
                    color: MUTED,
                    ..normal
                },
            );
        }
        job.wrap.max_width = f32::INFINITY;
        let painter = ui.painter().with_clip_rect(content_rect);
        let galley = painter.layout_job(job);
        painter.galley(
            Pos2::new(
                content_rect.left() - paint.offset,
                cell.center().y - galley.size().y / 2.0,
            ),
            galley,
            Color32::WHITE,
        );
    }
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals(egui::Visuals::dark());
    let mut style = (*ctx.style()).clone();
    style.spacing.button_padding = Vec2::new(11.0, 6.0);
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.visuals.selection.bg_fill = Color32::from_rgb(37, 82, 77);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    style.visuals.panel_fill = SURFACE;
    style.visuals.window_fill = Color32::from_rgb(28, 36, 48);
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.0));
    ctx.set_style(style);
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("cjk".into());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".into());
            break;
        }
    }
    ctx.set_fonts(fonts);
}
