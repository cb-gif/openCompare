use super::*;

impl CompareApp {
    pub(super) fn restore_comparison(&mut self, comparison: StoredComparison, ctx: &egui::Context) {
        self.options = comparison.options;
        self.encodings = [comparison.left_encoding, comparison.right_encoding];
        self.perform(
            Action::Open {
                left: comparison.left,
                right: comparison.right,
                directory: comparison.kind == ComparisonKind::Directory,
            },
            ctx,
        );
    }

    pub(super) fn remember_current(&mut self) {
        let comparison = match self.view {
            View::Text => self.session.as_ref().and_then(|session| {
                Some(StoredComparison {
                    kind: ComparisonKind::Text,
                    left: session.left.path.clone()?,
                    right: session.right.path.clone()?,
                    left_encoding: Some(session.left.format.encoding),
                    right_encoding: Some(session.right.format.encoding),
                    options: self.options,
                })
            }),
            View::Directory => self.directory.as_ref().map(|report| StoredComparison {
                kind: ComparisonKind::Directory,
                left: report.left.clone(),
                right: report.right.clone(),
                left_encoding: None,
                right_encoding: None,
                options: self.options,
            }),
        };
        if let Some(comparison) = comparison {
            self.state.remember(comparison);
        } else {
            self.state.last = None;
        }
        self.mark_state_dirty();
    }

    pub(super) fn mark_state_dirty(&mut self) {
        self.state.options = self.options;
        self.state.hide_equal = self.hide_equal;
        self.state
            .directory_filter
            .clone_from(&self.directory_filter);
        self.state_dirty_at = Some(Instant::now());
    }

    pub(super) fn flush_state(&mut self) {
        if self.state_dirty_at.take().is_none() || !self.state_writable {
            return;
        }
        if let Err(error) = self.state.save(&self.state_path) {
            self.error = Some(format!(
                "无法保存会话配置 {}：{error}",
                self.state_path.display()
            ));
        }
    }

    pub(super) fn recent_sessions(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new("最近会话").small().color(MUTED));
        let mut restore = None;
        for comparison in self.state.recent.iter().take(5) {
            let short_name = |path: &std::path::Path| {
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .chars()
                    .take(12)
                    .collect::<String>()
            };
            let kind = match comparison.kind {
                ComparisonKind::Text => "文本",
                ComparisonKind::Directory => "目录",
            };
            let label = format!("{kind}  {}", short_name(&comparison.left));
            if ui
                .button(RichText::new(label).size(12.0))
                .on_hover_text(format!(
                    "{}\n{}",
                    comparison.left.display(),
                    comparison.right.display()
                ))
                .clicked()
            {
                restore = Some(comparison.clone());
            }
        }
        if self.state.recent.is_empty() {
            ui.label(
                RichText::new("打开的比较将自动记录")
                    .size(11.0)
                    .color(MUTED),
            );
        }
        if let Some(comparison) = restore {
            self.guarded(Action::Restore(comparison), ui.ctx());
        }
    }
}
