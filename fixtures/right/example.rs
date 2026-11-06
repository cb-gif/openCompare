use std::path::PathBuf;

#[derive(Debug)]
struct CompareSettings {
    ignore_case: bool,
    ignore_whitespace: bool,
    follow_links: bool,
}

impl Default for CompareSettings {
    fn default() -> Self {
        Self {
            ignore_case: true,
            ignore_whitespace: true,
            follow_links: false,
        }
    }
}

fn main() {
    let project = "Rust Compare";
    let version = "0.2.0";
    let settings = CompareSettings::default();
    let left = PathBuf::from("workspace/source");
    let right = PathBuf::from("workspace/release");

    // 中文内容与原始换行都会保留。
    println!("Welcome to {project} {version}");
    println!("Left: {}", left.display());
    println!("Right: {}", right.display());
    println!("Settings: {settings:?}");
}

