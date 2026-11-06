use super::*;
use compare_fs::PreviewCursor;

#[derive(Default)]
pub(super) struct PreviewNavigation {
    pub history: Vec<[PreviewCursor; 2]>,
    pub current: usize,
    pub pending: Option<(usize, [PreviewCursor; 2])>,
}

impl CompareApp {
    pub(super) fn reset_preview_navigation(&mut self) {
        self.preview_navigation = PreviewNavigation::default();
        if self.session.as_ref().is_some_and(TextSession::is_read_only) {
            self.preview_navigation
                .history
                .push([PreviewCursor::default(); 2]);
        }
    }

    pub(super) fn preview_toolbar(&self, ui: &mut egui::Ui) -> Option<TextAction> {
        let session = self.session.as_ref()?;
        if !session.is_read_only() {
            return None;
        }
        let mut action = None;
        let more = [
            session.left.preview.as_ref(),
            session.right.preview.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|p| p.has_more());
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "只读预览 · 第 {} 页",
                    self.preview_navigation.current + 1
                ))
                .color(ACCENT),
            );
            let status = match session.byte_equal {
                Some(true) => "完整文件字节相同",
                Some(false) => "完整文件字节不同",
                None => "完整文件尚未比较",
            };
            ui.label(
                RichText::new(status).color(if session.byte_equal == Some(true) {
                    ACCENT
                } else {
                    Color32::from_rgb(245, 190, 115)
                }),
            );
            if ui
                .add_enabled(
                    self.busy.is_none() && self.preview_navigation.current > 0,
                    egui::Button::new("查看上页"),
                )
                .clicked()
            {
                action = Some(TextAction::Page(false));
            }
            if ui
                .add_enabled(self.busy.is_none() && more, egui::Button::new("查看下页"))
                .clicked()
            {
                action = Some(TextAction::Page(true));
            }
        });
        ui.label(RichText::new("差异和查找只覆盖当前页；忽略规则、编辑和合并暂停使用。完整字节比较结果来自打开或重新比较时。").small().color(MUTED));
        ui.add_space(6.0);
        action
    }

    pub(super) fn request_preview_page(&mut self, forward: bool) {
        if self.busy.is_some() {
            return;
        }
        let Some(session) = &self.session else { return };
        if !session.is_read_only() {
            return;
        }
        let current = self.preview_navigation.current;
        let index = if forward {
            current + 1
        } else {
            match current.checked_sub(1) {
                Some(index) => index,
                None => return,
            }
        };
        let cursors = if let Some(cursors) = self.preview_navigation.history.get(index) {
            *cursors
        } else {
            let (Some(left), Some(right)) = (&session.left.preview, &session.right.preview) else {
                return;
            };
            if !left.has_more() && !right.has_more() {
                return;
            }
            [left.end, right.end]
        };
        let job = Job::PreviewPage {
            documents: Box::new([session.left.clone(), session.right.clone()]),
            cursors,
            byte_equal: session.byte_equal.unwrap_or(false),
        };
        self.preview_navigation.pending = Some((index, cursors));
        self.search.refresh(None);
        self.submit(job, BusyKind::Open);
    }
}
