# Task 019 Review - Round 2

## 基本信息
- 审查时间: 2026-09-06
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/019-release-v0.1.0.md
- 审查范围: `dfa95d0` 的 Cargo.toml `[package].include` 改动及 Task 019 发布状态

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（398 passed，0 failed）
- cargo fmt --check: ✓
- cargo package --list: ⚠️ 当前工作区受 `.opencode/node_modules/` 未跟踪文件干扰，Cargo 要求 `--allow-dirty`；使用该选项时会列出环境文件。根据提交者在移开该环境目录后的验证，`include` 已排除 `docs/`，包内容符合白名单。

## 代码审查
### 通过项
1. `Cargo.toml:12-19` — 使用显式白名单限制发布内容，包含 `src/`、`tests/` 及 README、许可证、变更记录；不会再因 VCS 默认收集而发布内部 `docs/`。
2. 该改动未改变依赖、功能、源码或测试；Cargo 元数据位置正确，格式和项目约定一致。
3. 复跑四道门禁全部通过；测试总数未减少。

### 问题
1. [Critical] 仓库当前的 `v0.1.0` annotated tag 未指向包含本次修复的最终发布 commit。
   - 证据：`v0.1.0^{commit}` 为 `f15e283`，当前 HEAD 为 `dfa95d0`；而 `include` 改动只存在于 `dfa95d0`。
   - 影响：按 `docs/tasks/019-release-v0.1.0.md:42,84`，tag 标识的源码仍可能包含错误的打包配置，Task 019 的发布产物不可由 `v0.1.0` 唯一、正确地复现。
   - 建议：确认 `dfa95d0` 已是最终发布 commit 后，将 `v0.1.0` 移除并重新创建 annotated tag 指向该 commit，再按规格推送 tag；随后核验 `git cat-file -t v0.1.0` 为 `tag` 且 `git rev-parse v0.1.0^{commit}` 等于最终发布 commit。不要在未确认远端 tag 状态前强制覆盖。

## 建议
1. 建议在 CI 或发布脚本中用干净 checkout 执行 `cargo package --list`，并断言 `docs/`、`.git/`、`target/` 不出现，避免本地工具目录掩盖实际发布结果。
2. `chacha20 v0.10.1` 的 yanked 警告不是本次 Cargo.toml 改动引入的缺陷，但正式发布前建议单独更新锁文件并重新验证依赖可获取性。

## 结论
- [ ] 通过
- [x] 打回（Task 019 整体仍受 annotated tag 指向错误阻塞；Cargo.toml include 改动本身通过）

## 下一步
- @guigu-worker：Cargo.toml 改动无需修改。
- PM：完成 `v0.1.0` annotated tag 指向最终发布 commit 的修正后，再关闭 Task 019。
