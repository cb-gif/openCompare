use super::*;
use compare_fs::{DirectoryBatch, DirectoryPhase};

impl CompareApp {
    pub(super) fn apply_directory_batch(&mut self, batch: DirectoryBatch) {
        if matches!(
            self.directory_phase,
            Some(DirectoryPhase::Complete | DirectoryPhase::Cancelled | DirectoryPhase::Failed)
        ) {
            return;
        }
        self.directory_phase = Some(batch.phase);
        let report = self.directory.get_or_insert_with(|| DirectoryReport {
            left: batch.left.clone(),
            right: batch.right.clone(),
            entries: Vec::new(),
            complete: false,
        });
        for row in batch.entries {
            if let Some(index) = self.directory_index.get(&row.relative_path).copied() {
                report.entries[index] = row;
            } else {
                let index = report.entries.len();
                self.directory_index
                    .insert(row.relative_path.clone(), index);
                report.entries.push(row);
            }
        }
    }

    pub(super) fn cancel_current_job(&mut self) {
        if self.busy.as_ref().is_some_and(|b| b.kind == BusyKind::Copy) {
            self.worker.request_cancel();
            self.notice = "正在停止复制，请等待逐项结果".into();
            return;
        }
        if self
            .busy
            .as_ref()
            .is_some_and(|b| b.kind == BusyKind::Directory)
        {
            self.directory_phase = Some(DirectoryPhase::Cancelled);
            self.notice = "扫描已取消；当前结果不完整，请重新扫描后再复制".into();
        } else {
            self.notice = "任务已取消，已有会话仍保留".into();
        }
        self.worker.cancel();
        self.busy = None;
        self.preview_navigation.pending = None;
    }

    pub(super) fn directory_progress_label(&self, ui: &mut egui::Ui) {
        let Some(report) = &self.directory else {
            return;
        };
        if report.complete {
            return;
        }
        let label = match self.directory_phase {
            Some(DirectoryPhase::Discovering) => "正在发现条目；待比较项不代表相同或单侧文件",
            Some(DirectoryPhase::Comparing) => "正在比较内容；结果尚不完整",
            Some(DirectoryPhase::Cancelled) => "扫描已取消；结果不完整，文件复制暂不可用",
            Some(DirectoryPhase::Failed) => "扫描失败；结果不完整，文件复制暂不可用",
            _ => "正在准备扫描；结果尚不完整",
        };
        ui.label(RichText::new(label).color(Color32::from_rgb(245, 190, 115)));
    }
}
