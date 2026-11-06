use super::*;
use compare_core::{SearchMatch, SearchResults};
use std::{collections::BTreeMap, ops::Range};

struct LocatedMatch {
    hit: SearchMatch,
    row: usize,
}

#[derive(Default)]
pub(super) struct SearchState {
    pub visible: bool,
    pub query: String,
    pub case_sensitive: bool,
    pub focus_requested: bool,
    pub busy: bool,
    pub pending_jump: bool,
    worker: ComparisonWorker,
    hits: Vec<LocatedMatch>,
    selected: usize,
    truncated: bool,
    lines: BTreeMap<(u8, usize), Vec<Range<usize>>>,
}

impl SearchState {
    pub fn refresh(&mut self, session: Option<&TextSession>) {
        self.worker.cancel();
        self.hits.clear();
        self.lines.clear();
        self.selected = 0;
        self.truncated = false;
        self.pending_jump = false;
        self.busy = false;
        let Some(session) = session else { return };
        if !self.visible || self.query.is_empty() || session.diff.is_none() {
            return;
        }
        self.worker.submit(Job::Search {
            left: session.left.document.clone(),
            right: session.right.document.clone(),
            query: self.query.clone(),
            case_sensitive: self.case_sensitive,
        });
        self.busy = true;
    }

    pub fn poll(&mut self, session: Option<&TextSession>) {
        let Some(event) = self.worker.poll() else {
            return;
        };
        self.busy = false;
        let (Some(session), Ok(JobOutput::Searched(result))) = (session, event.result) else {
            return;
        };
        if result.cancelled
            || result.left_revision != session.left.document.revision
            || result.right_revision != session.right.document.revision
        {
            return;
        }
        self.install(session, result);
    }

    fn install(&mut self, session: &TextSession, result: SearchResults) {
        let Some(diff) = &session.diff else { return };
        let mut left_rows = vec![0; session.left.document.line_count()];
        let mut right_rows = vec![0; session.right.document.line_count()];
        for (index, row) in diff.rows.iter().enumerate() {
            if let Some(line) = row.left {
                left_rows[line] = index;
            }
            if let Some(line) = row.right {
                right_rows[line] = index;
            }
        }
        self.truncated = result.truncated;
        self.hits = result
            .matches
            .into_iter()
            .map(|hit| {
                let row = if hit.side == Side::Left {
                    left_rows[hit.line]
                } else {
                    right_rows[hit.line]
                };
                self.lines
                    .entry((side_key(hit.side), hit.line))
                    .or_default()
                    .push(hit.bytes.clone());
                LocatedMatch { hit, row }
            })
            .collect();
        self.hits
            .sort_by_key(|found| (found.row, side_key(found.hit.side), found.hit.bytes.start));
        self.selected = 0;
        self.pending_jump = !self.hits.is_empty();
    }

    pub fn step(&mut self, forward: bool) {
        if self.hits.is_empty() {
            return;
        }
        let count = self.hits.len();
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.pending_jump = true;
    }

    pub fn take_jump(&mut self) -> Option<usize> {
        if !std::mem::take(&mut self.pending_jump) {
            return None;
        }
        self.hits.get(self.selected).map(|found| found.row)
    }

    pub fn spans(&self, side: Side, line: usize) -> &[Range<usize>] {
        if !self.visible {
            return &[];
        }
        self.lines
            .get(&(side_key(side), line))
            .map_or(&[], Vec::as_slice)
    }

    pub fn active(&self, side: Side, line: usize) -> Option<&Range<usize>> {
        if !self.visible {
            return None;
        }
        self.hits
            .get(self.selected)
            .filter(|found| found.hit.side == side && found.hit.line == line)
            .map(|found| &found.hit.bytes)
    }
}

fn side_key(side: Side) -> u8 {
    if side == Side::Left {
        0
    } else {
        1
    }
}

impl CompareApp {
    pub(super) fn search_bar(&mut self, ui: &mut egui::Ui) {
        if !self.search.visible {
            return;
        }
        let mut close = false;
        let mut changed = false;
        let mut step = None;
        let preview = self.session.as_ref().is_some_and(TextSession::is_read_only);
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.search.query)
                    .id_salt("text-search")
                    .hint_text(if preview {
                        "查找当前页两侧文本…"
                    } else {
                        "查找两侧文本…"
                    })
                    .desired_width(270.0),
            );
            if self.search.focus_requested {
                response.request_focus();
                self.search.focus_requested = false;
            }
            changed |= response.changed();
            changed |= ui
                .checkbox(&mut self.search.case_sensitive, "区分大小写")
                .changed();
            let count = self.search.hits.len();
            if self.search.busy {
                ui.spinner();
            } else {
                ui.label(
                    RichText::new(format!(
                        "{} / {count}{}",
                        if count > 0 {
                            self.search.selected + 1
                        } else {
                            0
                        },
                        if self.search.truncated { "+" } else { "" }
                    ))
                    .small()
                    .color(MUTED),
                );
            }
            if ui
                .add_enabled(count > 0, egui::Button::new("上一个"))
                .clicked()
            {
                step = Some(false);
            }
            if ui
                .add_enabled(count > 0, egui::Button::new("下一个"))
                .clicked()
            {
                step = Some(true);
            }
            if ui.small_button("关闭查找").clicked() {
                close = true;
            }
            if (response.has_focus() || response.lost_focus())
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
            {
                step = Some(!ui.input(|i| i.modifiers.shift));
                response.request_focus();
            }
        });
        if changed {
            self.search.refresh(self.session.as_ref());
        }
        if let Some(forward) = step {
            self.search.step(forward);
        }
        if close {
            self.search.visible = false;
            self.search.refresh(None);
        }
        if self.search.truncated {
            ui.label(
                RichText::new("匹配超过 10,000 处，请缩小查找范围")
                    .small()
                    .color(MUTED),
            );
        }
        if let Some(row) = self.search.take_jump() {
            self.jump_row = Some(row);
            self.horizontal_offset = 0.0;
        }
        ui.add_space(6.0);
    }
}
