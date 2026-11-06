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
