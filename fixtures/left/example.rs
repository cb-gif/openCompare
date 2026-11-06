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
            ignore_case: false,
            ignore_whitespace: false,
            follow_links: true,
        }
    }
}

fn main() {
    let project = "Rust Compare";
    let version = "0.1.0";
    let settings = CompareSettings::default();
    let left = PathBuf::from("workspace/source");
    let right = PathBuf::from("workspace/backup");

    println!("Hello, {project} {version}");
    println!("Left: {}", left.display());
    println!("Right: {}", right.display());
    println!("Settings: {settings:?}");
}

