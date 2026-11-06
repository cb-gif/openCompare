//! Session history and a single, latest-request-wins background worker.
mod open;
mod persistence;
use compare_core::{
    compare, search_documents, CompareOptions, DiffResult, DocumentSnapshot, MergeError,
    SearchResults, Side,
};
use compare_fs::{
    compare_directories_streaming, execute_copy, prepare_copy, save_document, CopyPlan, CopyReport,
    DirectoryBatch, DirectoryReport, LoadedDocument, PreviewCursor, TextEncoding,
};
pub use open::{load_preview_session_page, open_text_session};
pub use persistence::{default_state_path, AppState, ComparisonKind, StoredComparison};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
        Arc, Condvar, Mutex,
    },
};

const HISTORY_LIMIT: usize = 64 * 1024 * 1024;

struct Edit {
    side: Side,
    before: Arc<str>,
    after: Arc<str>,
}
impl Edit {
    fn size(&self) -> usize {
        self.before.len() + self.after.len()
    }
}

pub struct TextSession {
    pub left: LoadedDocument,
    pub right: LoadedDocument,
    pub diff: Option<DiffResult>,
    /// Complete-file byte comparison; present only in bounded preview mode.
    pub byte_equal: Option<bool>,
    undo: VecDeque<Edit>,
    redo: Vec<Edit>,
    history_bytes: usize,
}

impl TextSession {
    pub fn new(left: LoadedDocument, right: LoadedDocument, options: CompareOptions) -> Self {
        let options = if left.preview.is_some() || right.preview.is_some() {
            CompareOptions::default()
        } else {
            options
        };
        let diff = compare(&left.document, &right.document, options);
        Self {
            left,
            right,
            diff: Some(diff),
            byte_equal: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
            history_bytes: 0,
        }
    }

    pub fn document(&self, side: Side) -> &LoadedDocument {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
    pub fn document_mut(&mut self, side: Side) -> &mut LoadedDocument {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }
    pub fn is_dirty(&self) -> bool {
        self.left.is_dirty() || self.right.is_dirty()
    }
    pub fn is_read_only(&self) -> bool {
        self.left.preview.is_some() || self.right.preview.is_some()
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn edit(&mut self, side: Side, text: impl Into<Arc<str>>) {
        if self.is_read_only() {
            return;
        }
        let text = text.into();
        let before = self.document(side).document.shared_text();
        if before == text {
            return;
        }
        let edit = Edit {
            side,
            before,
            after: text.clone(),
        };
        self.history_bytes += edit.size();
        self.undo.push_back(edit);
        self.redo.clear();
        while self.history_bytes > HISTORY_LIMIT && self.undo.len() > 1 {
            if let Some(edit) = self.undo.pop_front() {
                self.history_bytes -= edit.size();
            }
        }
        self.document_mut(side).replace_text(text);
        self.diff = None;
    }

    pub fn accept(&mut self, hunk: usize, source: Side) -> Result<(), MergeError> {
        if self.is_read_only() {
            return Err(MergeError::ReadOnly);
        }
        let diff = self.diff.as_ref().ok_or(MergeError::StaleResult)?;
        let text = diff.accept_hunk(hunk, source, &self.left.document, &self.right.document)?;
        let target = match source {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
        self.edit(target, text);
        Ok(())
    }

    pub fn undo(&mut self) {
        if let Some(edit) = self.undo.pop_back() {
            self.history_bytes -= edit.size();
            self.document_mut(edit.side)
                .replace_text(edit.before.clone());
            self.redo.push(edit);
            self.diff = None;
        }
    }

    pub fn redo(&mut self) {
        if let Some(edit) = self.redo.pop() {
            self.document_mut(edit.side)
                .replace_text(edit.after.clone());
            self.history_bytes += edit.size();
            self.undo.push_back(edit);
            self.diff = None;
        }
    }

    pub fn comparison_job(&self, options: CompareOptions) -> Job {
        Job::CompareText {
            left: self.left.document.clone(),
            right: self.right.document.clone(),
            options: if self.is_read_only() {
                CompareOptions::default()
            } else {
                options
            },
        }
    }

    pub fn install_diff(&mut self, diff: DiffResult) -> bool {
        if diff.left_revision != self.left.document.revision
            || diff.right_revision != self.right.document.revision
        {
            return false;
        }
        self.diff = Some(diff);
        true
    }
}

pub enum Job {
    PreviewPage {
        documents: Box<[LoadedDocument; 2]>,
        cursors: [PreviewCursor; 2],
        byte_equal: bool,
    },
    PrepareCopy {
        source_root: PathBuf,
        destination_root: PathBuf,
        paths: Vec<PathBuf>,
    },
    Copy(CopyPlan),
    Search {
        left: DocumentSnapshot,
        right: DocumentSnapshot,
        query: String,
        case_sensitive: bool,
    },
    OpenFiles {
        left: PathBuf,
        right: PathBuf,
        left_encoding: Option<TextEncoding>,
        right_encoding: Option<TextEncoding>,
        options: CompareOptions,
    },
    CompareText {
        left: DocumentSnapshot,
        right: DocumentSnapshot,
        options: CompareOptions,
    },
    CompareDirectories {
        left: PathBuf,
        right: PathBuf,
    },
    Save {
        side: Side,
        document: LoadedDocument,
    },
}

pub enum JobOutput {
    PreviewPage(Box<TextSession>),
    CopyPrepared(CopyPlan),
    Copied(CopyReport),
    Searched(SearchResults),
    Opened(Box<TextSession>),
    Compared(DiffResult),
    Directory(DirectoryReport),
    Saved {
        side: Side,
        document: LoadedDocument,
    },
}

pub struct JobResult {
    pub id: u64,
    pub result: Result<JobOutput, String>,
}

struct Pending {
    id: u64,
    job: Job,
    cancel: Arc<AtomicBool>,
}
struct DirectoryEvent {
    id: u64,
    batch: DirectoryBatch,
}
#[derive(Default)]
struct Mailbox {
    pending: Option<Pending>,
    active_cancel: Option<Arc<AtomicBool>>,
    shutdown: bool,
}

pub struct ComparisonWorker {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    results: Receiver<JobResult>,
    directory_updates: Receiver<DirectoryEvent>,
    pub progress: Arc<AtomicUsize>,
    pub current_id: u64,
}

impl Default for ComparisonWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl ComparisonWorker {
    pub fn new() -> Self {
        let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let progress = Arc::new(AtomicUsize::new(0));
        let (sender, results) = mpsc::channel();
        let (directory_sender, directory_updates) = mpsc::sync_channel(8);
        let shared_mailbox = mailbox.clone();
        let shared_progress = progress.clone();
        std::thread::Builder::new()
            .name("compare-worker".into())
            .spawn(move || loop {
                let request = {
                    let (lock, ready) = &*shared_mailbox;
                    let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
                    while state.pending.is_none() && !state.shutdown {
                        state = ready.wait(state).unwrap_or_else(|p| p.into_inner());
                    }
                    if state.shutdown {
                        break;
                    }
                    state.pending.take().expect("pending request was checked")
                };
                let must_report = matches!(&request.job, Job::Copy(_) | Job::Save { .. });
                if request.cancel.load(Ordering::Relaxed) && !must_report {
                    continue;
                }
                let id = request.id;
                let cancel = request.cancel.clone();
                let result = execute(request.job, &request.cancel, &shared_progress, |batch| {
                    let mut update = DirectoryEvent { id, batch };
                    loop {
                        match directory_sender.try_send(update) {
                            Ok(()) => break,
                            Err(mpsc::TrySendError::Full(returned)) => {
                                if cancel.load(Ordering::Relaxed) {
                                    break;
                                }
                                update = returned;
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                cancel.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                    }
                });
                if (must_report || !request.cancel.load(Ordering::Relaxed))
                    && sender
                        .send(JobResult {
                            id: request.id,
                            result,
                        })
                        .is_err()
                {
                    break;
                }
            })
            .expect("background worker could not be started");
        Self {
            mailbox,
            results,
            directory_updates,
            progress,
            current_id: 0,
        }
    }

    /// The mailbox holds at most one waiting job; obsolete work is cancelled and replaced.
    pub fn submit(&mut self, job: Job) -> u64 {
        self.current_id += 1;
        let (lock, ready) = &*self.mailbox;
        let mut state = lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cancel) = state.active_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.active_cancel = Some(cancel.clone());
        state.pending = Some(Pending {
            id: self.current_id,
            job,
            cancel,
        });
        self.progress.store(0, Ordering::Relaxed);
        ready.notify_one();
        self.current_id
    }

    pub fn cancel(&mut self) {
        self.current_id += 1;
        let mut state = self.mailbox.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cancel) = state.active_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        state.pending = None;
    }

    /// Request a cooperative stop while keeping the current job ID and its report.
    /// Use this for copy batches; `cancel` is for obsolete read-only work.
    pub fn request_cancel(&self) {
        let state = self.mailbox.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cancel) = &state.active_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn poll(&self) -> Option<JobResult> {
        while let Ok(result) = self.results.try_recv() {
            if result.id == self.current_id {
                return Some(result);
            }
        }
        None
    }

    pub fn poll_directory_updates(&self) -> Vec<DirectoryBatch> {
        let mut batches = Vec::new();
        for _ in 0..8 {
            match self.directory_updates.try_recv() {
                Ok(event) if event.id == self.current_id => batches.push(event.batch),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        batches
    }
}

impl Drop for ComparisonWorker {
    fn drop(&mut self) {
        self.cancel();
        let (lock, ready) = &*self.mailbox;
        lock.lock().unwrap_or_else(|p| p.into_inner()).shutdown = true;
        ready.notify_one();
    }
}

fn execute(
    job: Job,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
    mut directory_update: impl FnMut(DirectoryBatch),
) -> Result<JobOutput, String> {
    match job {
        Job::PreviewPage {
            documents,
            cursors,
            byte_equal,
        } => load_preview_session_page(&documents[0], &documents[1], cursors, byte_equal, cancel)
            .map(|session| JobOutput::PreviewPage(Box::new(session)))
            .map_err(|e| e.to_string()),
        Job::PrepareCopy {
            source_root,
            destination_root,
            paths,
        } => prepare_copy(&source_root, &destination_root, &paths, cancel, |count| {
            progress.store(count, Ordering::Relaxed)
        })
        .map(JobOutput::CopyPrepared)
        .map_err(|e| e.to_string()),
        Job::Copy(plan) => execute_copy(&plan, cancel, |count| {
            progress.store(count, Ordering::Relaxed)
        })
        .map(JobOutput::Copied)
        .map_err(|e| e.to_string()),
        Job::Search {
            left,
            right,
            query,
            case_sensitive,
        } => Ok(JobOutput::Searched(search_documents(
            &left,
            &right,
            &query,
            case_sensitive,
            || cancel.load(Ordering::Relaxed),
        ))),
        Job::OpenFiles {
            left,
            right,
            left_encoding,
            right_encoding,
            options,
        } => open_text_session(
            &left,
            &right,
            left_encoding,
            right_encoding,
            options,
            cancel,
        )
        .map(|session| JobOutput::Opened(Box::new(session)))
        .map_err(|e| e.to_string()),
        Job::CompareText {
            left,
            right,
            options,
        } => Ok(JobOutput::Compared(compare(&left, &right, options))),
        Job::CompareDirectories { left, right } => {
            compare_directories_streaming(&left, &right, cancel, |batch| {
                progress.store(batch.scanned, Ordering::Relaxed);
                directory_update(batch);
            })
            .map(JobOutput::Directory)
            .map_err(|e| e.to_string())
        }
        Job::Save { side, mut document } => {
            save_document(&mut document).map_err(|e| e.to_string())?;
            Ok(JobOutput::Saved { side, document })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_merge_undo_and_redo_preserve_documents() {
        let mut session = TextSession::new(
            LoadedDocument::from_text("left\r\n"),
            LoadedDocument::from_text("right\n"),
            CompareOptions::default(),
        );
        session.accept(0, Side::Left).unwrap();
        assert_eq!(session.right.document.text(), "left\r\n");
        assert!(session.right.is_dirty());
        assert!(session.diff.is_none());
        session.undo();
        assert_eq!(session.right.document.text(), "right\n");
        assert!(!session.is_dirty());
        session.redo();
        assert_eq!(session.right.document.text(), "left\r\n");
    }

    #[test]
    fn old_results_cannot_replace_current_alignment() {
        let mut session = TextSession::new(
            LoadedDocument::from_text("a"),
            LoadedDocument::from_text("b"),
            CompareOptions::default(),
        );
        let old_diff = session.diff.clone().unwrap();
        session.edit(Side::Right, "c");
        assert!(!session.install_diff(old_diff));
        assert!(session.diff.is_none());
    }

    #[test]
    fn editing_after_undo_discards_redo_branch() {
        let mut session = TextSession::new(
            LoadedDocument::from_text("a"),
            LoadedDocument::from_text("b"),
            CompareOptions::default(),
        );
        session.edit(Side::Right, "c");
        session.undo();
        session.edit(Side::Right, "d");
        assert!(!session.can_redo());
        session.undo();
        assert_eq!(session.right.document.text(), "b");
    }

    #[test]
    fn background_worker_only_delivers_the_latest_requested_revision() {
        let mut worker = ComparisonWorker::new();
        for revision in 0..32 {
            worker.submit(Job::CompareText {
                left: DocumentSnapshot::new("left\n", revision),
                right: DocumentSnapshot::new("right\n", revision),
                options: CompareOptions::default(),
            });
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(event) = worker.poll() {
                assert_eq!(event.id, worker.current_id);
                match event.result.unwrap() {
                    JobOutput::Compared(diff) => assert_eq!(diff.left_revision, 31),
                    _ => panic!("unexpected result type"),
                }
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not complete"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_full_directory_update_queue_does_not_block_cancellation_or_publish_stale_batches() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        for index in 0..1200 {
            std::fs::write(left.path().join(format!("item-{index}")), "x").unwrap();
        }
        let mut worker = ComparisonWorker::new();
        worker.submit(Job::CompareDirectories {
            left: left.path().to_owned(),
            right: right.path().to_owned(),
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while worker.progress.load(Ordering::Relaxed) < 1024 {
            assert!(
                std::time::Instant::now() < deadline,
                "scan did not fill the bounded update queue"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        worker.cancel();
        worker.submit(Job::CompareText {
            left: DocumentSnapshot::new("latest", 1),
            right: DocumentSnapshot::new("latest", 1),
            options: CompareOptions::default(),
        });
        loop {
            if let Some(result) = worker.poll() {
                let JobOutput::Compared(diff) = result.result.unwrap() else {
                    panic!("stale directory result published")
                };
                assert!(diff.hunks.is_empty());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cancellation did not unblock the worker"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(worker.poll_directory_updates().is_empty());
    }

    #[test]
    fn cancelling_a_copy_keeps_the_completion_report_deliverable() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("a"), vec![42u8; 256_000]).unwrap();
        let plan = prepare_copy(
            source.path(),
            destination.path(),
            &["a".into()],
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut worker = ComparisonWorker::new();
        let id = worker.submit(Job::Copy(plan));
        worker.request_cancel();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(event) = worker.poll() {
                assert_eq!(event.id, id);
                let JobOutput::Copied(report) = event.result.unwrap() else {
                    panic!("expected a copy report")
                };
                assert_eq!(report.items.len(), 1);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "copy report was lost");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
