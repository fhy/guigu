# Task 017-c Review - Round 1

## 基本信息
- 审查时间: 2026-09-06
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/017-c-lock-discipline-cleanup.md
- 审查提交: e66461d（当前 HEAD）

## 门禁结果
- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（267 个库测试及全部集成测试通过）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Critical] `src/plugin/mod.rs:126-137` — 017-c 要求 `PluginRegistry::tools()` 在释放注册表读锁后再调用外部 `plugin.tools()`，但当前实现仍在 `let guard = self.plugins.read()` 的生命周期内执行 `plugin.tools()`（133 行）。这会把外部插件回调置于注册表锁内，回调尝试 `register/unregister` 可能阻塞/死锁，未满足规格第 28-33、71 条。
   - 影响: 插件回调不受信任时破坏锁纪律，且读锁期间阻塞会阻塞写操作。
   - 建议: 锁内复制并按 id 排序 `Vec<(String, Arc<dyn Plugin>)>`，离开锁作用域后遍历该向量调用 `plugin.tools()`；保持插件 id 字典序和声明序。

2. [Critical] `src/plugin/tool.rs:29-39` — 未实现规格要求的 `PluginTool::try_new`，因此无法在构造时校验 `spec.name` 是否由插件声明；当前 `new` 仍是唯一构造入口且没有一致性校验。
   - 影响: 外部调用者可以构造 schema 与 plugin 不一致的工具，错误延迟到 `execute`，公开契约与验收标准第 73 条不符。
   - 建议: 增加 `pub fn try_new(...) -> Result<Self, PluginError>`，调用 `plugin.tools()` 检查名称命中并在未命中时返回 `ToolNotDeclared`；保留 `new`，但明确其仅供注册表遍历声明集合的内部路径使用。

3. [Critical] `src/tools/file_mutation_queue.rs:7-11,20-26,41-57` — 未实现 `FileMutationQueue::prune()`、自动阈值驱逐及相关测试；当前锁表仍只增不减，且模块文档明确保留已由 017-c 要求移除的“只增不减”局限。
   - 影响: 长时间运行、持续访问大量不同路径时 `locks` 无界增长；规格第 74 条的 guard/in-flight/阈值/驱逐后互斥回归均没有实现或验证。
   - 建议: 增加公开 `prune()`，在锁表锁内 retain `Arc::strong_count(lock) > 1`；`acquire` 持表锁后、插入前达到阈值时执行等价的 locked helper（不要在已持 `std::sync::Mutex` 时递归调用公开 `prune()`，避免死锁），并补齐验收要求的测试。

4. [Critical] `src/acp/tests.rs:1-723`、`src/acp/tests_transport.rs:1-413` — ACP 测试拆分未实施。两个文件分别超过 400 行上限，`tests.rs` 虽未超过 30 个测试，但仍违反规格第 54-55、75 条；当前 `src/acp/mod.rs:28-35` 也没有新增拆分模块声明。
   - 影响: 文件体量门禁失败，测试按职责维护困难。
   - 建议: 仅重组测试代码，拆成职责清晰的子模块/文件；保持测试总数不减，并确保每个测试文件 ≤400 行、≤30 个 `#[test]`。

### 建议
1. 当前工作树除 `docs/reviews/017-b-review-r1.md` 外没有 017-c 的源码或测试改动；该 untracked 文件不属于本轮审查范围，不应随本审查报告暂存。
2. 017-c 新增锁纪律测试应覆盖插件回调内 `try_write()`，以实际证明回调发生在 registry 读锁释放后，而不只依赖代码走读。

## 规格核对
- `tools()` 回调移出锁：✗
- `PluginTool::try_new`：✗
- `FileMutationQueue` 显式/自动驱逐：✗
- ACP 测试文件体量拆分：✗
- 既有功能及四道门禁：✓（基线通过）

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请实现并提交上述 4 项 Critical 修复后重新申请 Task 017-c 审查。
