//! Portable session metadata. Unsaved document contents are never persisted here.
use compare_core::CompareOptions;
use compare_fs::TextEncoding;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const STATE_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_RECENT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComparisonKind {
    Text,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredComparison {
    pub kind: ComparisonKind,
    pub left: PathBuf,
    pub right: PathBuf,
    #[serde(default)]
    pub left_encoding: Option<TextEncoding>,
    #[serde(default)]
    pub right_encoding: Option<TextEncoding>,
    #[serde(default)]
    pub options: CompareOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub version: u32,
    pub last: Option<StoredComparison>,
    pub recent: Vec<StoredComparison>,
    pub options: CompareOptions,
    pub hide_equal: bool,
    pub directory_filter: String,
    pub window_size: [f32; 2],
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            last: None,
            recent: Vec::new(),
            options: CompareOptions::default(),
            hide_equal: true,
            directory_filter: String::new(),
            window_size: [1280.0, 820.0],
        }
    }
}

pub fn default_state_path() -> PathBuf {
    if let Some(directory) = std::env::var_os("RUST_COMPARE_STATE_DIR") {
        return PathBuf::from(directory).join("sessions.json");
    }
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rust-compare"));
    executable
        .parent()
        .unwrap_or(Path::new("."))
        .join(".rust-compare")
        .join("sessions.json")
}

impl AppState {
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(format!("无法读取会话配置 {}：{error}", path.display())),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("会话配置超过 1 MiB，原文件已保留".into());
        }
        let mut state: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("会话配置无效，原文件已保留：{e}"))?;
        if state.version != STATE_VERSION {
            return Err(format!(
                "不支持的会话配置版本 {}，原文件已保留",
                state.version
            ));
        }
        state.recent.truncate(MAX_RECENT);
        if !state.window_size[0].is_finite() || !state.window_size[1].is_finite() {
            state.window_size = Self::default().window_size;
        }
        state.window_size[0] = state.window_size[0].clamp(900.0, 5000.0);
        state.window_size[1] = state.window_size[1].clamp(600.0, 3000.0);
        Ok(state)
    }

    pub fn remember(&mut self, comparison: StoredComparison) {
        self.recent.retain(|old| {
            old.kind != comparison.kind
                || old.left != comparison.left
                || old.right != comparison.right
        });
        self.recent.insert(0, comparison.clone());
        self.recent.truncate(MAX_RECENT);
        self.last = Some(comparison);
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err("会话配置超过 1 MiB，未写入".into());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        temporary.write_all(&bytes).map_err(|e| e.to_string())?;
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        temporary.persist(path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comparison(index: usize) -> StoredComparison {
        StoredComparison {
            kind: ComparisonKind::Text,
            left: PathBuf::from(format!("左侧{index}.txt")),
            right: PathBuf::from(format!("右侧{index}.txt")),
            left_encoding: Some(TextEncoding::Gbk),
            right_encoding: None,
            options: CompareOptions {
                ignore_case: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn session_metadata_round_trips_without_document_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state/sessions.json");
        let mut state = AppState {
            hide_equal: false,
            directory_filter: "中文".into(),
            ..Default::default()
        };
        state.remember(comparison(1));
        state.save(&path).unwrap();
        let loaded = AppState::load(&path).unwrap();
        assert_eq!(loaded.last, state.last);
        assert_eq!(loaded.recent, state.recent);
        assert!(!loaded.hide_equal);
        assert_eq!(loaded.directory_filter, "中文");
        state.remember(comparison(2));
        state.save(&path).unwrap();
        assert_eq!(AppState::load(&path).unwrap().last, Some(comparison(2)));
    }

    #[test]
    fn recent_sessions_are_deduplicated_and_bounded() {
        let mut state = AppState::default();
        for index in 0..20 {
            state.remember(comparison(index));
        }
        state.remember(comparison(15));
        assert_eq!(state.recent.len(), MAX_RECENT);
        assert_eq!(state.recent[0], comparison(15));
        assert_eq!(
            state
                .recent
                .iter()
                .filter(|c| **c == comparison(15))
                .count(),
            1
        );
    }

    #[test]
    fn corrupt_or_future_settings_are_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        assert!(AppState::load(&path).unwrap().last.is_none());
        for data in [b"broken".as_slice(), b"{\"version\":999}"] {
            fs::write(&path, data).unwrap();
            assert!(AppState::load(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), data);
        }
    }
}
