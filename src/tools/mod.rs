//! 内置工具集（Task 005/006）：read / write / edit / bash / echo / deferred。
//!
//! 017-b：文件工具工作目录显式化——`resolve_tool_path` 把工具路径解析为归一化
//! 绝对路径（绝对路径不变；相对路径 join 注入的 `work_dir`，`work_dir` 为
//! `None` 时按进程 cwd 解析，保持旧行为），解析结果同用于 `FileMutationQueue`
//! 锁 key 与 IO，工具不再隐式依赖进程 cwd（session 间隔离）。

use std::path::{Path, PathBuf};

pub mod bash;
pub mod deferred;
pub mod echo;
pub mod edit;
pub mod file_mutation_queue;
pub mod read;
pub mod write;

pub use bash::BashTool;
pub use deferred::{DeferredTool, DeferredToolSpec};
pub use echo::EchoTool;
pub use edit::EditTool;
pub use file_mutation_queue::{FileMutationGuard, FileMutationQueue};
pub use read::ReadTool;
pub use write::WriteTool;

/// 把工具路径解析为归一化绝对路径（017-b）：绝对路径不变；相对路径 join
/// `work_dir`（`work_dir` 为 `None` 时按进程 cwd 解析，保持旧行为）。
///
/// 解析只做一次，结果同用于 `FileMutationQueue` 锁 key 与 IO，保证锁 key 与
/// 实际写文件路径一致。`std::path::absolute` 失败（空路径等）退回 join 结果，
/// 由后续 IO 返回明确错误（不 panic）。
pub(crate) fn resolve_tool_path(work_dir: Option<&Path>, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(dir) = work_dir {
        dir.join(p)
    } else {
        p.to_path_buf()
    };
    std::path::absolute(&joined).unwrap_or(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 绝对路径不受 work_dir 影响（不变）。
    #[test]
    fn test_resolve_absolute_unchanged() {
        let p = resolve_tool_path(Some(Path::new("/work")), "/abs/file.txt");
        assert_eq!(p, PathBuf::from("/abs/file.txt"));
    }

    /// 相对路径 join work_dir。
    #[test]
    fn test_resolve_relative_joins_work_dir() {
        let p = resolve_tool_path(Some(Path::new("/work")), "sub/file.txt");
        assert_eq!(p, PathBuf::from("/work/sub/file.txt"));
    }

    /// work_dir 为 None：相对路径按进程 cwd 解析（结果为绝对路径，保持旧行为）。
    #[test]
    fn test_resolve_relative_no_work_dir() {
        let p = resolve_tool_path(None, "file.txt");
        assert!(p.is_absolute(), "should resolve against process cwd");
        assert!(p.ends_with("file.txt"));
    }

    /// work_dir 为相对路径时，join 结果再按进程 cwd 归一化为绝对路径。
    #[test]
    fn test_resolve_relative_work_dir() {
        let p = resolve_tool_path(Some(Path::new("sub")), "file.txt");
        assert!(p.is_absolute());
        assert!(p.ends_with("sub/file.txt"));
    }
}
