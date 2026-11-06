use super::*;
use compare_fs::{CopyItemStatus, CopyPlan, CopyReport};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct CopyUi {
    pub selection: BTreeSet<usize>,
    pub preview: Option<CopyPlan>,
    pub report: Option<CopyReport>,
}

impl CompareApp {
    pub(super) fn can_copy_selection(&self, source: Side) -> bool {
        if self.busy.is_some()
            || self.session.as_ref().is_some_and(TextSession::is_dirty)
            || self.copy.selection.is_empty()
        {
            return false;
        }
        let Some(report) = &self.directory else {
            return false;
        };
        self.copy.selection.iter().all(|index| {
            let Some(entry) = report.entries.get(*index) else {
                return false;
            };
            let (from, to) = if source == Side::Left {
                (entry.left_kind, entry.right_kind)
            } else {
                (entry.right_kind, entry.left_kind)
            };
            from == Some(EntryKind::File)
                && matches!(to, None | Some(EntryKind::File))
                && entry.status != EntryStatus::Error
        })
    }

    pub(super) fn prepare_selected_copy(&mut self, source: Side) {
        if !self.can_copy_selection(source) {
            return;
        }
        let Some(report) = &self.directory else {
            return;
        };
        let (source_root, destination_root) = if source == Side::Left {
            (report.left.clone(), report.right.clone())
        } else {
            (report.right.clone(), report.left.clone())
        };
        let paths = self
            .copy
            .selection
            .iter()
            .map(|index| report.entries[*index].relative_path.clone())
            .collect();
        self.submit(
            Job::PrepareCopy {
                source_root,
                destination_root,
                paths,
            },
            BusyKind::CopyPlanning,
        );
    }

    pub(super) fn execute_copy_preview(&mut self) {
        if let Some(plan) = self.copy.preview.take() {
            self.submit(Job::Copy(plan), BusyKind::Copy);
        }
    }

    pub(super) fn copy_toolbar(&mut self, ui: &mut egui::Ui) {
        let mut direction = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.can_copy_selection(Side::Left),
                    egui::Button::new("复制到右侧…"),
                )
                .clicked()
            {
                direction = Some(Side::Left);
            }
            if ui
                .add_enabled(
                    self.can_copy_selection(Side::Right),
                    egui::Button::new("复制到左侧…"),
                )
                .clicked()
            {
                direction = Some(Side::Right);
            }
            if ui.button("选择可见文件").clicked() {
                self.copy.selection.clear();
                if let Some(report) = &self.directory {
                    self.copy
                        .selection
                        .extend(self.filtered_entries.iter().copied().filter(|index| {
                            let entry = &report.entries[*index];
                            (entry.left_kind == Some(EntryKind::File)
                                || entry.right_kind == Some(EntryKind::File))
                                && !matches!(
                                    entry.status,
                                    EntryStatus::Error | EntryStatus::TypeConflict
                                )
                        }));
                }
            }
            if ui.button("清除选择").clicked() {
                self.copy.selection.clear();
            }
            ui.label(
                RichText::new(format!(
                    "已选 {} 项 · Ctrl+单击多选",
                    self.copy.selection.len()
                ))
                .small()
                .color(MUTED),
            );
        });
        if self.session.as_ref().is_some_and(TextSession::is_dirty) {
            ui.label(
                RichText::new("请先保存或撤销文本修改，再执行目录复制")
                    .small()
                    .color(Color32::from_rgb(245, 190, 115)),
            );
        }
        if let Some(source) = direction {
            self.prepare_selected_copy(source);
        }
    }

    pub(super) fn finish_copy(&mut self, report: CopyReport) {
        let copied = report.copied_count();
        self.notice = format!(
            "复制{} · 成功 {copied} 项 · 失败 {} 项",
            if report.cancelled {
                "已停止"
            } else {
                "完成"
            },
            report.failed_count()
        );
        // A clean file session affected by replacement must not keep an obsolete baseline.
        if let Some(session) = &self.session {
            let affected = [session.left.path.as_ref(), session.right.path.as_ref()]
                .into_iter()
                .flatten()
                .any(|path| {
                    let canonical = std::fs::canonicalize(path).ok();
                    report
                        .items
                        .iter()
                        .filter(|item| item.status == CopyItemStatus::Copied)
                        .any(|item| {
                            canonical.as_ref()
                                == Some(&report.destination_root.join(&item.relative_path))
                        })
                });
            if affected {
                self.session = None;
                self.search.refresh(None);
            }
        }
        let (left, right) = self.directory.as_ref().map_or_else(
            || (report.source_root.clone(), report.destination_root.clone()),
            |current| (current.left.clone(), current.right.clone()),
        );
        self.copy.report = Some(report);
        self.copy.selection.clear();
        self.submit(Job::CompareDirectories { left, right }, BusyKind::Directory);
    }

    pub(super) fn copy_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(plan) = &self.copy.preview {
            let mut execute = false;
            let mut cancel = false;
            egui::Window::new("确认文件复制")
                .collapsible(false)
                .resizable(true)
                .default_width(650.0)
                .show(ctx, |ui| {
                    ui.label(format!("来源：{}", plan.source_root.display()));
                    ui.label(format!("目标：{}", plan.destination_root.display()));
                    ui.add_space(8.0);
                    ui.label(format!(
                        "{} 个文件 · {} 项覆盖 · {} 字节",
                        plan.operations.len(),
                        plan.overwrite_count(),
                        plan.total_bytes()
                    ));
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("copy-preview")
                        .max_height(260.0)
                        .show_rows(ui, 28.0, plan.operations.len(), |ui, range| {
                            for operation in &plan.operations[range] {
                                copy_row(
                                    ui,
                                    &operation.relative_path,
                                    if operation.overwrites {
                                        "覆盖"
                                    } else {
                                        "新建"
                                    },
                                    if operation.overwrites {
                                        Color32::from_rgb(245, 190, 115)
                                    } else {
                                        ACCENT
                                    },
                                    &format!("{} B", operation.bytes),
                                    None,
                                );
                            }
                        });
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "执行前会重新检查内容。取消后保留已复制的文件，并显示逐项结果。",
                        )
                        .small()
                        .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("返回").clicked() {
                            cancel = true;
                        }
                        if ui
                            .add(egui::Button::new("执行复制").fill(Color32::from_rgb(35, 92, 81)))
                            .clicked()
                        {
                            execute = true;
                        }
                    });
                });
            if cancel {
                self.copy.preview = None;
            }
            if execute {
                self.execute_copy_preview();
            }
        }
        if let Some(report) = &self.copy.report {
            let mut visible = true;
            egui::Window::new("文件复制结果")
                .collapsible(false)
                .default_width(650.0)
                .open(&mut visible)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "{} · 成功 {} 项 · 失败 {} 项",
                        if report.cancelled {
                            "已取消后续复制"
                        } else {
                            "复制批次完成"
                        },
                        report.copied_count(),
                        report.failed_count()
                    ));
                    ui.label(RichText::new("目录视图会自动刷新").small().color(MUTED));
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("copy-results")
                        .max_height(320.0)
                        .show_rows(ui, 28.0, report.items.len(), |ui, range| {
                            for item in &report.items[range] {
                                let (label, color) = match &item.status {
                                    CopyItemStatus::Copied => ("已复制", ACCENT),
                                    CopyItemStatus::Failed(_) => {
                                        ("失败", Color32::from_rgb(245, 157, 151))
                                    }
                                    CopyItemStatus::Cancelled => ("复制中取消", MUTED),
                                    CopyItemStatus::NotRun => ("未执行", MUTED),
                                };
                                let error = if let CopyItemStatus::Failed(error) = &item.status {
                                    Some(error.as_str())
                                } else {
                                    None
                                };
                                copy_row(ui, &item.relative_path, label, color, "", error);
                            }
                        });
                });
            if !visible {
                self.copy.report = None;
            }
        }
    }
}

fn copy_row(
    ui: &mut egui::Ui,
    path: &std::path::Path,
    status: &str,
    color: Color32,
    detail: &str,
    error: Option<&str>,
) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        Pos2::new(rect.left(), rect.center().y),
        egui::Align2::LEFT_CENTER,
        status,
        FontId::proportional(12.0),
        color,
    );
    let path_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 88.0, rect.top()),
        Pos2::new(
            (rect.right() - 100.0).max(rect.left() + 88.0),
            rect.bottom(),
        ),
    );
    ui.painter().with_clip_rect(path_rect).text(
        path_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        path.to_string_lossy(),
        FontId::proportional(13.0),
        Color32::from_rgb(213, 222, 231),
    );
    painter.text(
        Pos2::new(rect.right(), rect.center().y),
        egui::Align2::RIGHT_CENTER,
        detail,
        FontId::proportional(12.0),
        MUTED,
    );
    response.on_hover_text(error.map_or_else(
        || path.display().to_string(),
        |error| format!("{}\n{error}", path.display()),
    ));
}
