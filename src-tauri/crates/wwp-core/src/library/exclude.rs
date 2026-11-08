//! 排除规则匹配：**只有"目录前缀"一类**。
//!
//! ## 为什么只剩一类
//!
//! 原先还有 glob（通配符）与正则，配套的是界面上的「排除规则」页。用户明确
//! 不会去那一页编辑规则——他排除目录的方式是**图库页的目录树复选框**，
//! 而复选框改的始终只有目录前缀这一类。
//!
//! 留着另外两类意味着：三套匹配语义、三套测试、一个没人用的界面，
//! 以及"两条规则同时对同一批文件生效"这种难以向用户解释的状态。
//! 因此按用户要求删掉，`regex` 依赖也一并从 Cargo.toml 移除。
//!
//! 语义：`D:/pics/excluded` 命中它自己与它的**所有子目录**（整棵子树跳过）。
//! 这是扫描时最省时间的一类——在目录层面就剪枝，根本不会走进去。

use std::path::{Path, PathBuf};

use crate::config::ExcludeSettings;

/// 构造好的排除规则。
#[derive(Debug, Default)]
pub struct ExcludeMatcher {
    /// 已规范化为 `/` 分隔、且已转小写的目录前缀。
    dir_prefixes: Vec<String>,
}

impl ExcludeMatcher {
    pub fn new(settings: &ExcludeSettings) -> ExcludeMatcher {
        let dir_prefixes = settings
            .dirs
            .iter()
            .map(|dir| normalize(dir))
            .filter(|dir| !dir.is_empty())
            .collect();
        ExcludeMatcher { dir_prefixes }
    }

    pub fn is_empty(&self) -> bool {
        self.dir_prefixes.is_empty()
    }

    /// 目录是否应整棵跳过。
    pub fn should_skip_dir(&self, dir: &Path) -> Option<ExcludedBy> {
        if self.dir_prefixes.is_empty() {
            return None;
        }
        let normalized = normalize(&dir.to_string_lossy());
        for prefix in &self.dir_prefixes {
            if normalized == *prefix || normalized.starts_with(&format!("{prefix}/")) {
                return Some(ExcludedBy::Dir(prefix.clone()));
            }
        }
        None
    }

    /// 文件是否位于被排除的目录之下。
    ///
    /// 逐级向上找父目录：用户直接问"这张图为什么没出现"时给出答案，
    /// 而匹配的判据与扫描时**完全一致**（同一个 `should_skip_dir`），
    /// 不会出现"扫描说排除了、这里说没排除"。
    pub fn matches_file(&self, path: &Path) -> Option<ExcludedBy> {
        let mut current = path.parent();
        while let Some(dir) = current {
            if let Some(hit) = self.should_skip_dir(dir) {
                return Some(hit);
            }
            current = dir.parent();
        }
        None
    }
}

/// 命中的是哪条规则——界面需要告诉用户"为什么这张图没出现"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExcludedBy {
    Dir(String),
}

impl ExcludedBy {
    pub fn describe(&self) -> String {
        match self {
            ExcludedBy::Dir(pattern) => format!("目录规则：{pattern}"),
        }
    }
}

/// 统一分隔符并转小写。
///
/// Windows 路径不分大小写，因此比较一律在小写域里做。
pub fn normalize(text: &str) -> String {
    let replaced = text.replace('\\', "/");
    let trimmed = replaced.trim_end_matches('/');
    // 去掉 `\\?\` 前缀，否则带前缀与不带前缀的同一条路径会被判为不同。
    let without_prefix = trimmed
        .strip_prefix("//?/")
        .map(|rest| rest.to_string())
        .unwrap_or_else(|| trimmed.to_string());
    without_prefix.to_ascii_lowercase()
}

/// 给界面用的"这条路径为什么被排除"的即时测试。
pub fn explain(settings: &ExcludeSettings, path: &Path) -> Option<ExcludedBy> {
    ExcludeMatcher::new(settings).matches_file(path)
}

/// 把规则文本转成 PathBuf（供 dirs 规则使用）。
pub fn as_path(text: &str) -> PathBuf {
    PathBuf::from(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dirs: &[&str]) -> ExcludeSettings {
        ExcludeSettings {
            dirs: dirs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn dir_prefix_matches_itself_and_children_only() {
        let matcher = ExcludeMatcher::new(&settings(&[r"D:\Pics\excluded"]));
        assert!(matcher
            .should_skip_dir(Path::new(r"D:\Pics\excluded"))
            .is_some());
        assert!(matcher
            .should_skip_dir(Path::new(r"D:\Pics\excluded\screenshots"))
            .is_some());
        // 大小写与分隔符不同也应命中。
        assert!(matcher
            .should_skip_dir(Path::new("d:/pics/EXCLUDED/deep"))
            .is_some());
        // 前缀相似但不是子目录，不能误伤。
        assert!(matcher
            .should_skip_dir(Path::new(r"D:\Pics\excluded-other"))
            .is_none());
        assert!(matcher.should_skip_dir(Path::new(r"D:\Pics")).is_none());
    }

    #[test]
    fn a_file_is_excluded_when_any_parent_directory_matches() {
        let matcher = ExcludeMatcher::new(&settings(&[r"D:\Pics\screenshots"]));
        assert!(matcher
            .matches_file(Path::new(r"D:\Pics\screenshots\2024\a.jpg"))
            .is_some());
        assert!(matcher.matches_file(Path::new(r"D:\Pics\a.jpg")).is_none());
    }

    #[test]
    fn long_path_prefix_does_not_change_matching() {
        let matcher = ExcludeMatcher::new(&settings(&[r"D:\Pics\excluded"]));
        assert!(matcher
            .should_skip_dir(Path::new(r"\\?\D:\Pics\excluded\deep"))
            .is_some());
    }

    #[test]
    fn explain_reports_the_matching_directory_rule() {
        let config = settings(&[r"D:\Pics\screenshots"]);
        let hit = explain(&config, Path::new(r"D:\Pics\screenshots\2024\a.jpg")).unwrap();
        assert_eq!(hit.describe(), "目录规则：d:/pics/screenshots");
        assert!(explain(&config, Path::new(r"D:\Pics\a.jpg")).is_none());
    }

    #[test]
    fn empty_rules_exclude_nothing() {
        let matcher = ExcludeMatcher::new(&settings(&[]));
        assert!(matcher.is_empty());
        assert!(matcher.should_skip_dir(Path::new(r"D:\any")).is_none());
        assert!(matcher.matches_file(Path::new(r"D:\any\a.jpg")).is_none());
    }
}
