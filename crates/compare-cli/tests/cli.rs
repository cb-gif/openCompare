use std::{fs, process::Command};

#[test]
fn cli_exit_codes_and_json_are_usable_for_automation() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left.txt");
    let right = dir.path().join("right.txt");
    fs::write(&left, "Hello\r\n").unwrap();
    fs::write(&right, "hello\n").unwrap();
    let invoke = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bcompare"))
            .arg("diff")
            .arg(&left)
            .arg(&right)
            .args(extra)
            .output()
            .unwrap()
    };
    let different = invoke(&["--json"]);
    assert_eq!(different.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&different.stdout).unwrap();
    assert_eq!(parsed["equal"], false);
    let ignored = invoke(&["--ignore-case", "--ignore-line-endings", "--json"]);
    assert_eq!(ignored.status.code(), Some(0));
    fs::remove_file(&right).unwrap();
    assert_eq!(invoke(&[]).status.code(), Some(2));
}

#[test]
fn preview_exit_status_uses_complete_bytes_even_when_the_first_page_is_identical() {
    let directory = tempfile::tempdir().unwrap();
    let left = directory.path().join("left.txt");
    let right = directory.path().join("right.txt");
    let text = "line\n".repeat(compare_fs::MAX_EDITABLE_LINES + 1);
    fs::write(&left, &text).unwrap();
    fs::write(&right, text + "tail\n").unwrap();
    let invoke = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_bcompare"))
            .arg("diff")
            .arg(&left)
            .arg(&right)
            .args(extra)
            .output()
            .unwrap()
    };
    let different = invoke(&["--json"]);
    assert_eq!(different.status.code(), Some(1));
    let result: serde_json::Value = serde_json::from_slice(&different.stdout).unwrap();
    assert_eq!(result["equal"], false);
    assert_eq!(result["mode"], "read_only_preview");
    assert_eq!(result["comparison_scope"], "full_file_bytes");
    assert_eq!(result["hunk_scope"], "first_page");
    assert_eq!(result["hunks"].as_array().unwrap().len(), 0);
    assert_eq!(result["left_preview"]["has_more"], true);
    let ignored = invoke(&["--ignore-case"]);
    assert_eq!(ignored.status.code(), Some(2));
    assert!(String::from_utf8(ignored.stderr)
        .unwrap()
        .contains("remove ignore options"));
    fs::copy(&left, &right).unwrap();
    assert_eq!(invoke(&["--json"]).status.code(), Some(0));
}
