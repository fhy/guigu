# Task 017-c Review - Round 2

## 基本信息
- 审查时间: 2026-09-06
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/017-c-lock-discipline-cleanup.md
- 审查提交: 76a2e71

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（274 个库测试及全部集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 修复确认
1. `src/plugin/mod.rs:130-148` — `tools()` 在读锁内仅复制并排序 `(id, Arc<dyn Plugin>)`，锁释放后才调用 `plugin.tools()`；新增 `try_write()` 探针测试验证锁纪律，满足确定性顺序和重入要求。
2. `src/plugin/tool.rs:45-56` — 新增 `PluginTool::try_new`，按声明名称校验并返回 `ToolNotDeclared`；`new` 保留且明确限定为注册表内部装配路径，测试覆盖成功与失败分支。
3. `src/tools/file_mutation_queue.rs:23-95` — 新增 `PRUNE_THRESHOLD`、显式 `prune()`、`len()`/`is_empty()` 及锁内 `prune_locked`；阈值触发在插入前执行，避免递归获取表锁。测试覆盖释放条目、in-flight 条目、自动驱逐及驱逐后互斥。
4. ACP 测试已按场景拆分：`tests.rs` 301 行、`tests_session_load.rs` 273 行、`tests_mapping.rs` 175 行、`tests_transport.rs` 205 行、`tests_transport_errors.rs` 220 行，均未超过 400 行或 30 个测试，测试总数保持并通过。

## 问题
无 Critical / Warning 问题。

## 建议
无阻塞性建议。`src/tools/file_mutation_queue.rs:83` 的 `prune_locked` 接收 `&self` 但当前不使用 self，可选地改为关联函数以表达其纯 helper 语义；不影响正确性，非本任务阻塞项。

## 规格核对
- `tools()` 回调移出锁：✓
- `PluginTool::try_new`：✓
- `FileMutationQueue` 显式/自动驱逐：✓
- ACP 测试文件体量拆分：✓
- 既有功能及四道门禁：✓

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- Task 017-c 可标记为完成。
