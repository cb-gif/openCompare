use super::*;
use std::{fs, thread};

fn wait_for_jobs(app: &mut CompareApp) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll();
        app.search.poll(app.session.as_ref());
        if app.busy.is_none() && !app.search.busy {
            break;
        }
        assert!(Instant::now() < deadline, "background job did not complete");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        app.error.is_none(),
        "{}",
        app.error.as_deref().unwrap_or_default()
    );
}

fn render_text(app: &mut CompareApp, ctx: &egui::Context) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 820.0))),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.text_view(ui));
        },
    )
}

fn shape_contains(shape: &egui::Shape, needle: &str) -> bool {
    match shape {
        egui::Shape::Text(text) => text.galley.job.text.contains(needle),
        egui::Shape::Vec(shapes) => shapes.iter().any(|shape| shape_contains(shape, needle)),
        _ => false,
    }
}

#[test]
fn copy_preview_execution_refresh_and_saved_session_work_together() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left");
    let right = directory.path().join("right");
    fs::create_dir_all(left.join("nested")).unwrap();
    fs::create_dir(&right).unwrap();
    fs::write(left.join("sample.txt"), "中文 source\n").unwrap();
    fs::write(right.join("sample.txt"), "old\n").unwrap();
    fs::write(left.join("nested/new.bin"), vec![155u8; 150_000]).unwrap();
    let ctx = egui::Context::default();
    let path = directory.path().join("state/sessions.json");
    let mut app = CompareApp::initialize(&ctx, AppState::default(), path.clone(), None, &[]);
    app.perform(
        Action::Open {
            left: left.clone(),
            right: right.clone(),
            directory: true,
        },
        &ctx,
    );
    wait_for_jobs(&mut app);
    let report = app.directory.as_ref().unwrap();
    app.copy.selection.extend(
        report
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.left_kind == Some(EntryKind::File))
            .map(|(index, _)| index),
    );
    assert!(app.can_copy_selection(Side::Left));
    assert!(!app.can_copy_selection(Side::Right));
    app.prepare_selected_copy(Side::Left);
    wait_for_jobs(&mut app);
    let preview = app.copy.preview.as_ref().unwrap();
    assert_eq!(preview.operations.len(), 2);
    assert_eq!(preview.overwrite_count(), 1);
    assert_eq!(
        fs::read_to_string(right.join("sample.txt")).unwrap(),
        "old\n"
    );
    assert!(!right.join("nested").exists());
    let _ = ctx.run(egui::RawInput::default(), |ctx| app.copy_dialogs(ctx));
    app.execute_copy_preview();
    wait_for_jobs(&mut app);
    assert_eq!(app.copy.report.as_ref().unwrap().copied_count(), 2);
    assert_eq!(app.directory.as_ref().unwrap().different_count(), 0);
    assert!(app.filtered_entries.is_empty());
    assert_eq!(
        fs::read(left.join("nested/new.bin")).unwrap(),
        fs::read(right.join("nested/new.bin")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(right.join("sample.txt")).unwrap(),
        "中文 source\n"
    );
    app.flush_state();
    let saved = AppState::load(&path).unwrap();
    assert_eq!(saved.last.as_ref().unwrap().kind, ComparisonKind::Directory);
    drop(app);
    let mut restored = CompareApp::initialize(&ctx, saved, path, None, &[]);
    wait_for_jobs(&mut restored);
    assert!(restored.view == View::Directory);
    assert_eq!(restored.directory.as_ref().unwrap().different_count(), 0);
}

#[test]
fn text_restart_restores_paths_encoding_rules_and_preferences() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left.txt");
    let right = directory.path().join("右侧.txt");
    fs::write(&left, "中文 HELLO\n").unwrap();
    fs::write(
        &right,
        compare_fs::encode(
            "中文 hello\n",
            compare_fs::TextFormat {
                encoding: TextEncoding::Gbk,
                bom: false,
            },
        )
        .unwrap(),
    )
    .unwrap();
    let options = CompareOptions {
        ignore_case: true,
        ..Default::default()
    };
    let mut state = AppState {
        options,
        hide_equal: false,
        directory_filter: "中文".into(),
        ..Default::default()
    };
    state.remember(StoredComparison {
        kind: ComparisonKind::Text,
        left: left.clone(),
        right: right.clone(),
        left_encoding: Some(TextEncoding::Utf8),
        right_encoding: Some(TextEncoding::Gbk),
        options,
    });
    let path = directory.path().join("sessions.json");
    state.save(&path).unwrap();
    let ctx = egui::Context::default();
    let mut app = CompareApp::initialize(
        &ctx,
        AppState::load(&path).unwrap(),
        path.clone(),
        None,
        &[],
    );
    wait_for_jobs(&mut app);
    let session = app.session.as_ref().unwrap();
    assert!(app.view == View::Text);
    assert_eq!(session.left.path.as_ref(), Some(&left));
    assert_eq!(session.right.path.as_ref(), Some(&right));
    assert_eq!(session.right.document.text(), "中文 hello\n");
    assert_eq!(session.right.format.encoding, TextEncoding::Gbk);
    assert!(session.diff.as_ref().unwrap().hunks.is_empty());
    assert!(!app.hide_equal);
    assert_eq!(app.directory_filter, "中文");
    assert_eq!(app.options, options);
    assert_eq!(app.state.recent.len(), 1);
    app.flush_state();
    drop(app);
    let mut restored =
        CompareApp::initialize(&ctx, AppState::load(&path).unwrap(), path, None, &[]);
    wait_for_jobs(&mut restored);
    assert_eq!(restored.encodings[1], Some(TextEncoding::Gbk));
    assert!(restored
        .session
        .as_ref()
        .unwrap()
        .diff
        .as_ref()
        .unwrap()
        .hunks
        .is_empty());
}

#[test]
fn long_line_search_navigation_renders_the_original_unicode_match() {
    let directory = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut app = CompareApp::initialize(
        &ctx,
        AppState::default(),
        directory.path().join("state.json"),
        None,
        &[],
    );
    let text = "x".repeat(5000) + "中文👩‍💻\n";
    app.session = Some(TextSession::new(
        LoadedDocument::from_text(&text),
        LoadedDocument::from_text(&text),
        CompareOptions::default(),
    ));
    app.handle_text_action(TextAction::Find);
    app.search.query = "中文".into();
    app.search.refresh(app.session.as_ref());
    wait_for_jobs(&mut app);
    let expected_range = 5000..5006;
    assert_eq!(
        app.search.spans(Side::Left, 0),
        std::slice::from_ref(&expected_range)
    );
    assert_eq!(app.search.active(Side::Left, 0), Some(&expected_range));
    let _ = render_text(&mut app, &ctx);
    let output = render_text(&mut app, &ctx);
    assert!(output
        .shapes
        .iter()
        .any(|shape| shape_contains(&shape.shape, "中文👩‍💻")));
    app.search.step(true);
    assert_eq!(app.search.active(Side::Right, 0), Some(&expected_range));
    assert_eq!(app.search.take_jump(), Some(0));
}

#[test]
fn invalid_configuration_is_kept_when_the_gui_cannot_persist_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.json");
    fs::write(&path, "invalid configuration").unwrap();
    let error = AppState::load(&path).unwrap_err();
    let ctx = egui::Context::default();
    let mut app = CompareApp::initialize(&ctx, AppState::default(), path.clone(), Some(error), &[]);
    app.mark_state_dirty();
    app.flush_state();
    assert_eq!(fs::read_to_string(path).unwrap(), "invalid configuration");
    assert!(app.error.is_some());
    assert!(app.state_dirty_at.is_none());
}

#[test]
fn read_only_pages_navigate_in_both_directions_after_one_side_reaches_eof() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("large.txt");
    let right = directory.path().join("short.txt");
    fs::write(&left, "same\n".repeat(compare_fs::MAX_EDITABLE_LINES + 1)).unwrap();
    fs::write(&right, "same\n".repeat(compare_fs::PREVIEW_MAX_LINES + 1)).unwrap();
    let ctx = egui::Context::default();
    let mut app = CompareApp::initialize(
        &ctx,
        AppState::default(),
        directory.path().join("state.json"),
        None,
        &[],
    );
    app.perform(
        Action::Open {
            left: left.clone(),
            right: right.clone(),
            directory: false,
        },
        &ctx,
    );
    wait_for_jobs(&mut app);
    let session = app.session.as_ref().unwrap();
    assert!(session.is_read_only());
    assert_eq!(session.byte_equal, Some(false));
    assert!(session.diff.as_ref().unwrap().hunks.is_empty());
    app.handle_text_action(TextAction::Edit(Side::Right));
    app.handle_text_action(TextAction::Save(Side::Right));
    app.handle_text_action(TextAction::Accept(Side::Left));
    assert!(app.editor.is_none());
    assert!(app.busy.is_none());
    let output = render_text(&mut app, &ctx);
    assert!(output
        .shapes
        .iter()
        .any(|shape| shape_contains(&shape.shape, "完整文件字节不同")));
    for expected_page in 1..=2 {
        app.handle_text_action(TextAction::Page(true));
        wait_for_jobs(&mut app);
        assert_eq!(app.preview_navigation.current, expected_page);
    }
    assert!(app
        .session
        .as_ref()
        .unwrap()
        .right
        .document
        .text()
        .is_empty());
    app.handle_text_action(TextAction::Page(false));
    wait_for_jobs(&mut app);
    assert_eq!(
        app.session.as_ref().unwrap().right.document.text(),
        "same\n"
    );
    let output = render_text(&mut app, &ctx);
    assert!(output
        .shapes
        .iter()
        .any(|shape| shape_contains(&shape.shape, "2001")));
    app.handle_text_action(TextAction::Page(false));
    wait_for_jobs(&mut app);
    assert_eq!(app.preview_navigation.current, 0);
    assert!(app
        .session
        .as_ref()
        .unwrap()
        .diff
        .as_ref()
        .unwrap()
        .hunks
        .is_empty());
    assert_eq!(
        fs::read_to_string(&right).unwrap(),
        "same\n".repeat(compare_fs::PREVIEW_MAX_LINES + 1)
    );
    fs::write(&left, "external change").unwrap();
    app.handle_text_action(TextAction::Page(true));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.busy.is_some() {
        app.poll();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert!(app.error.is_some());
    assert_eq!(app.preview_navigation.current, 0);
    assert_eq!(app.session.as_ref().unwrap().byte_equal, Some(false));
}

#[test]
fn incremental_scan_can_be_cancelled_filtered_and_restarted_without_enabling_copy() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left");
    let right = directory.path().join("right");
    fs::create_dir(&left).unwrap();
    fs::create_dir(&right).unwrap();
    for i in 0..1200 {
        fs::write(left.join(format!("file-{i:04}.txt")), "same").unwrap();
    }
    let ctx = egui::Context::default();
    let mut app = CompareApp::initialize(
        &ctx,
        AppState::default(),
        directory.path().join("state.json"),
        None,
        &[],
    );
    let scan = || Job::CompareDirectories {
        left: left.clone(),
        right: right.clone(),
    };
    app.submit(scan(), BusyKind::Directory);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll();
        if app
            .directory
            .as_ref()
            .is_some_and(|r| !r.entries.is_empty())
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert!(app.busy.is_some());
    assert!(!app.directory.as_ref().unwrap().complete);
    app.directory_filter = app.directory.as_ref().unwrap().entries[0]
        .relative_path
        .to_string_lossy()
        .to_uppercase();
    app.refresh_directory_filter();
    assert_eq!(app.filtered_entries.len(), 1);
    app.copy.selection.insert(app.filtered_entries[0]);
    assert!(!app.can_copy_selection(Side::Left));
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.directory_view(ui));
    });
    app.cancel_current_job();
    assert_eq!(app.directory_phase, Some(DirectoryPhase::Cancelled));
    assert!(!app.directory.as_ref().unwrap().complete);
    assert!(!app.can_copy_selection(Side::Left));
    let retained = app.directory.as_ref().unwrap().entries.len();
    app.poll();
    assert_eq!(app.directory.as_ref().unwrap().entries.len(), retained);
    app.submit(scan(), BusyKind::Directory);
    wait_for_jobs(&mut app);
    assert!(app.directory.as_ref().unwrap().complete);
    assert_eq!(app.directory.as_ref().unwrap().entries.len(), 1200);
    assert_eq!(app.directory_phase, Some(DirectoryPhase::Complete));
    assert_eq!(app.filtered_entries.len(), 1);
    app.copy.selection.insert(app.filtered_entries[0]);
    assert!(app.can_copy_selection(Side::Left));
    app.apply_directory_batch(compare_fs::DirectoryBatch {
        left,
        right,
        phase: DirectoryPhase::Discovering,
        scanned: 0,
        entries: Vec::new(),
    });
    assert_eq!(app.directory_phase, Some(DirectoryPhase::Complete));
}
