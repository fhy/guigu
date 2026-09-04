# Task 017-c: 锁纪律收尾（插件锁 + FileMutationQueue 驱逐 + 测试拆分）

## Background

016 交付插件机制，006 交付 `FileMutationQueue`，014 交付 ACP 适配。三处审查各留下非阻塞建议，均属「锁纪律 / 资源回收 / 测试体量」类收尾项，本任务集中处理：

1. **016 r1-1**：`PluginRegistry::tools()` 在持 `RwLock` 读锁期间调用外部 `plugin.tools()`，外部回调置于注册表锁内。
2. **016 r1-2**：`PluginTool::new` 无运行时一致性校验（`spec.name` 是否被 plugin 声明）。
3. **006 遗留**：`FileMutationQueue` 锁表只增不减（无界增长），需安全驱逐。
4. **014 r4-1**：`src/acp/tests.rs`（476 行）、`src/acp/tests_transport.rs`（413 行）略超 400 行单文件上限。

## Goal

- `PluginRegistry::tools()` 把外部回调移出注册表锁。
- 新增 `PluginTool::try_new`，提供一致性校验入口。
- `FileMutationQueue` 增加安全的锁表驱逐（含自动触发阈值 + 显式 `prune()`）。
- 拆分 ACP 测试文件至 ≤ 400 行 / ≤ 30 `#[test]`。

## Design Notes

### 契约复用（勿改）

- `Plugin` trait（016 定稿）：`id() / tools() -> Vec<DeferredToolSpec> / async instantiate(name) -> Result<Arc<dyn Tool>, PluginError>`。
- `PluginError::{ DuplicatePlugin, PluginNotFound, ToolNotDeclared, Instantiation }`（016 定稿）。
- `PluginTool`（016 定稿）：`tokio::sync::OnceCell<Arc<dyn Tool>>` 异步惰性、失败不缓存。
- `FileMutationQueue`（006 定稿）：`std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>` 惰性建锁，`acquire` 返回 `FileMutationGuard`（`OwnedMutexGuard`）。

### 1. `PluginRegistry::tools()` 移出锁

- `tools()` 在 `RwLock` 读锁内**仅**复制 `(id, Arc<dyn Plugin>)` 并按 id 字典序排序为 `Vec<(String, Arc<dyn Plugin>)>`，随即释放读锁。
- 释放锁后遍历该向量，对每个 plugin 调 `plugin.tools()` 并按声明序包成 `PluginTool`，产出 `Vec<Arc<dyn Tool>>`。
- 顺序确定性不变：插件 id 字典序 × 声明序（排序在锁内完成，组装在锁外完成，顺序不依赖 HashMap 迭代序）。
- 不把外部 `plugin.tools()` / `instantiate` 置于注册表锁内。

### 2. `PluginTool::try_new`

- 新增 `pub fn try_new(plugin: Arc<dyn Plugin>, spec: DeferredToolSpec) -> Result<Self, PluginError>`：
  - 校验 `plugin.tools()` 中存在 `spec.name` 对应 spec（名称命中即通过）；否则返回 `PluginError::ToolNotDeclared(spec.name)`。
- `new` 保留：doc 明确「不做一致性校验，仅供 `PluginRegistry::tools()` 遍历声明集合时内部使用；外部构造应优先 `try_new`」。
- `tools()` 组装路径继续用 `new`（遍历声明集合，天然一致），不引入 fallible 组装、不改 `tools()` 返回签名。

### 3. `FileMutationQueue` 锁表驱逐

- 新增 `pub fn prune(&self)`：在锁表 `std::sync::Mutex` 锁内，`map.retain(|_, lock| Arc::strong_count(lock) > 1)` —— 仅移除只剩 map 一份强引用的条目。
- **正确性依据**：`acquire` 的「clone Arc」与 `prune` 的「check strong_count + remove」都在同一把锁表锁内串行，故二者原子：
  - 有 in-flight acquire 或持有中的 guard 时，`strong_count ≥ 2`，`prune` 跳过；
  - `strong_count == 1` 时无任何在途引用，移除后旧锁强引用归零即回收，后续 acquire 新建锁，互斥不被破坏。
  - 故**无需** 006 规格所述「两阶段 dying 态 / 代际计数」——单段「锁内 check-and-remove」即安全（006 的竞态仅在其假设「check 与 remove 不原子」时成立）。
- **自动触发**：`acquire` 在拿锁表锁后、`or_insert_with` 前，若 `map.len() >= THRESHOLD`（常量，如 1024，`const PRUNE_THRESHOLD: usize = 1024`），先执行一次 `prune()`。
- 更新模块 doc：移除「锁表只增不减」的已知局限声明，改为「惰性驱逐：`strong_count==1` 条目在阈值触发或显式 `prune()` 时回收」。

### 4. ACP 测试文件拆分

- `src/acp/tests.rs`（476 行）与 `src/acp/tests_transport.rs`（413 行）按场景拆分，每个文件 ≤ 400 行且 ≤ 30 `#[test]`。
- 拆分仅重组测试代码（`mod tests` / 子模块），**不改产品逻辑、不删测试**。按职责自然切分（如 transport 拆 stdio / SSE / framing / 错误路径），命名沿用既有风格。

## Files

- src/plugin/mod.rs（`tools()` 移出锁 + 单测）
- src/plugin/tool.rs（`try_new` + 单测）
- src/tools/file_mutation_queue.rs（`prune()` + 阈值触发 + 单测 + doc 更新）
- src/acp/tests.rs、src/acp/tests_transport.rs（按场景拆分，可新增子模块文件）
- tests/（如插件一致性 / queue 驱逐集成测试）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] `tools()` 锁内仅复制排序 `(id, Arc)`，`plugin.tools()` 在锁外调用（可用「插件回调内尝试获取注册表写锁」的测试断言不死锁/不阻塞，或代码审查确认）
- [ ] `tools()` 组装顺序仍确定：插件 id 字典序 × 声明序
- [ ] `PluginTool::try_new` 对未声明 name 返回 `ToolNotDeclared`；对已声明 name 成功；`tools()` 组装路径不受影响
- [ ] `FileMutationQueue::prune()`：全部 guard drop 后 `prune()` 清空锁表（`len == 0`）；有 in-flight guard 时该 path 不被驱逐；`acquire` 达阈值自动 prune；驱逐后同 path 再 acquire 仍互斥（回归测试）
- [ ] ACP 测试文件全部 ≤ 400 行且 ≤ 30 `#[test]`，测试总数不减、全绿
- [ ] 产品代码无 `unwrap()`；异步测试用 `tokio::test`；单文件 ≤ 400 行

## 修订记录

- v1.0（2026-09-05，Architect）：初稿。打包 016 r1 两条（`tools()` 移出注册表锁、`PluginTool::try_new` 一致性校验）、006 锁表驱逐（`prune()` + 阈值，锁内 `strong_count` check-and-remove 原子，无需两阶段 dying）、014 测试文件拆分三项收尾。均不改变既有公开契约语义（`tools()` 返回签名、`new` 保留、queue 互斥语义不变）。
