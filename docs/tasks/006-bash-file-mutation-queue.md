# Task 006: bash 工具 + file_mutation_queue

## Background

005 已交付三个文件工具（read/write/edit），均为**单 agent 内**安全：write/edit 的 `FileWriter` 串行由 003 主循环顺序编排保证。但多个 `AgentRuntime` 实例（多 agent）共享同一进程、写**同一文件**时，003 的编排无法覆盖——两个 agent 可并发写同一路径、互相覆盖。本任务落地 003 已预留的 `file_mutation_queue`（跨 agent 同文件写串行化底座），并补上 `Exclusive` 语义的 bash 工具，验证独占编排。

## Goal

- 新增 `FileMutationQueue`：进程内、跨 agent 的 **per-path 异步写锁**，供 write/edit 注入串行化
- 改造 `WriteTool` / `EditTool`：构造时注入 `Arc<FileMutationQueue>`，写 IO 前 acquire、写后 RAII 释放
- 新增 `BashTool`（`resource_scope = Exclusive`）：真实子进程执行 + 取消 kill + 超时
- 单元测试（queue 互斥/并行/释放/取消）+ bash 工具测试 + 既有 tools 测试构造处同步更新

## Design Notes

### 契约复用（以既有定稿为准，勿改）

- `Tool` trait 完整签名（003 定稿，`on_update` 为 `&(dyn Fn(ToolResult) + Send + Sync)`，勿写成旧式 `&dyn Fn`）：
  `execute(&self, tool_call_id: &str, args: serde_json::Value, signal: CancellationToken, on_update: Option<&(dyn Fn(ToolResult) + Send + Sync)>) -> Result<ToolResult, ToolError>`
- `ResourceScope::{ ReadOnly, FileWriter, Exclusive }`（core/tool.rs 定稿）
- `ToolError { message }` 构造器：`invalid_arguments(String)`、`new(String)`（005 已核验，本任务复用，不改 core/tool.rs）
- `ToolResult { content, is_error, details }` 便捷构造：`text(..)` / `error(..)`（005 已核验）
- 工具落 `src/tools/`，经 `AgentRuntime { tools: Vec<Arc<dyn Tool>> }` 注册（003 定稿，本任务不新增注册字段）
- tokio 已启用 `features = ["full"]`（含 `process`/`time`），**无需改 Cargo.toml**

### FileMutationQueue（src/tools/file_mutation_queue.rs）

```rust
pub struct FileMutationQueue { /* 内部：per-path 锁表 */ }

impl FileMutationQueue {
    pub fn new() -> Self;
    /// 获取 path 的写锁；不同 path 可并行，同一 path 串行。
    /// 等待期间可被外层 `tokio::select!` + `signal.cancelled()` 打断（本方法自身不绑定取消）。
    pub async fn acquire(&self, path: &Path) -> FileMutationGuard<'_>;
}

pub struct FileMutationGuard<'a> { /* 持有锁，Drop 自动释放 */ }
```

**关键语义**：

1. **锁粒度 per-path**：以规范化后的路径为 key，不同文件并行、同一文件互斥。规范化用 `std::path::absolute(path)`（失败则退回原始 `PathBuf` 作 key）；**一期不解析 symlink/hardlink**（同一物理文件经不同路径可能漏串行化，属已知局限，规格接受）。
2. **RAII 释放**：`FileMutationGuard` Drop 即释放，覆盖异常/取消提前返回路径，不要求调用方显式 release。
3. **可跨 await 持有**：guard 必须 `Send` 且可在文件 IO 的 `await` 期间持有。建议内部用 `Arc<tokio::sync::Mutex<()>>` + `lock_owned()` 得到 `OwnedMutexGuard`，规避借用 lifetime 复杂度（实现方式 Developer 可自定，但须满足 Send + 跨 await）。
4. **惰性建锁**：按需为每个 path 建锁；锁表的并发访问用 `std::sync::Mutex`（锁表操作极短，不跨 await）。**一期锁表只增不减**：安全驱逐需两阶段 dying 态或代际计数（否则有竞态——A drop 后查 `strong_count==1` 准备移除，B 此刻 acquire 同 path 克隆旧 Arc（count=2）持旧锁进临界区，A 移除表项后 C 新 acquire 建新锁，B（旧锁）与 C（新锁）并发进临界区，互斥被破坏）；条目小（约 100–200B）、agent 触碰路径通常有界，故接受无界增长，与 symlink 局限并列声明为已知局限，后续任务按需再补。

### WriteTool / EditTool 改造

- 构造改为持锁注入：
  - `WriteTool::new(queue: Arc<FileMutationQueue>) -> Self`
  - `EditTool::new(queue: Arc<FileMutationQueue>) -> Self`
- 写路径时序（在既有 005 校验逻辑基础上插入 acquire，其余行为不变）：
  1. 入口 `signal.is_cancelled()` 检查（沿用 005）
  2. 反序列化 / 参数校验（沿用 005）
  3. **acquire 可取消**：`let _guard = tokio::select! { g = queue.acquire(&path) => g, _ = signal.cancelled() => return Err(ToolError::new("cancelled".into())) };`
  4. **拿锁后二次取消检查**（消除 acquire 等待期间被取消的竞态）→ 已取消则返回取消 `ToolError`
  5. 文件 IO（`create_dir_all` / `read` / `write`，沿用 005）
  6. `_guard` 作用域结束自动释放
- 其余 `name`/`description`/`parameters`/`resource_scope`/`details` 均与 005 完全一致，不重定义

### BashTool（src/tools/bash.rs）

- `name = "bash"`，`resource_scope = Exclusive`（单 agent 内独占由 003 主循环保证，无需 bash 自己再排队）
- `parameters()`：
  ```json
  { "type": "object",
    "properties": {
      "command":    { "type": "string" },
      "cwd":        { "type": "string" },
      "timeout_ms": { "type": "integer", "minimum": 1 } },
    "required": ["command"] }
  ```
- args：`BashArgs { command: String, cwd: Option<String>, timeout_ms: Option<u64> }`
- 行为：
  1. 入口 `signal.is_cancelled()` 检查 → 取消 `ToolError`
  2. 反序列化失败 → `invalid_arguments`
  3. 以 `sh -c <command>` 启动子进程（**用 `sh` 而非 `bash`**，POSIX 可移植，且支持管道/重定向语义）；`Command` 设 `kill_on_drop(true)` 防泄漏；`cwd` 有值时设置
  4. `spawn()` 拿 `Child`；先 `take()` 出 `child.stdout` / `child.stderr`（`Option<ChildStdout>` / `Option<ChildStderr>`），若有则 `tokio::spawn` 并行 `read_to_end` 排空（避免管道缓冲写满死锁）；再用 `tokio::select!` 三路等待。**禁用 `wait_with_output`**：它按值消费 `Child`（`mut self`），而 `select!` 急切创建各分支 future，`child` 移入完成分支后，取消/超时分支的 `child.kill()` 变 use-after-move（E0382）；先 `wait()` 再 `wait_with_output()` 亦不可行（`wait()` 已 reap）：
     - `child.wait()` 完成（`&mut self`，不移动 child）→ join 排空任务拿 stdout/stderr → 按退出码组装结果
     - `signal.cancelled()` → `child.kill().await` 后 `child.wait().await`（严格 reap）→ 返回取消 `ToolError`（消息含 "cancelled"）
     - `sleep(timeout_ms)`（仅当设置了超时）→ `child.kill().await` 后 `child.wait().await`（严格 reap）→ 返回超时 `ToolError`（消息含 "timeout"）
  5. **非零退出码不 throw**（Pi 哲学"错误不 throw"）：返回 `Ok(ToolResult { is_error: true, content: stderr 或组合文本, details: {"exit_code": n, "stdout": .., "stderr": ..} })`
  6. 成功（exit 0）→ `Ok(ToolResult::text(stdout))`，`details: {"exit_code": 0}`
- **必须 kill 且 reap 子进程**：取消/超时路径显式 `kill().await` 后 `wait().await`（严格 reap，不依赖 tokio best-effort reaper——`ChildDropGuard::drop` 只 `kill()` 不 reap，官方对孤儿进程回收仅 best-effort、不保证及时性），配合 `kill_on_drop(true)` 兜底，不得泄漏僵尸进程

### 错误语义统一

- 参数非法 → `ToolError::invalid_arguments`
- 无法 spawn / IO / 进程 wait 异常 → `ToolError::new`
- 取消 / 超时 → `ToolError::new`（消息分别含 "cancelled" / "timeout"），由 003 主循环统一产出 `stop_reason: Aborted`，工具本身不直接改 stop_reason
- bash **命令非零退出**是"命令执行了但业务失败"→ 走 `ToolResult::is_error`，**不是** `ToolError`（这是与 write/edit 的关键区别）

### 边界声明（明确不做，避免过度设计）

- file_mutation_queue 为**进程内**串行化；跨进程（多 guigu 进程写同一文件）需文件锁/flock，属后续 session/远程协议范畴，不在本任务
- bash 的**跨 agent 独占**（bash 与其它 agent 的 write/edit 互斥）需全局读写锁 + per-path 锁的层级结构，超出一期"同文件写串行化"范围，不在本任务；本任务 bash 的 `Exclusive` 仅保证**单 agent 内**不与写工具并行（003 已实现）

## Files

- src/tools/file_mutation_queue.rs（FileMutationQueue + FileMutationGuard + 单元测试）
- src/tools/bash.rs（BashTool + BashArgs + 单元测试）
- src/tools/write.rs（WriteTool 改造：注入 queue + acquire + 同步调整单元测试构造）
- src/tools/edit.rs（EditTool 改造：注入 queue + acquire + 同步调整单元测试构造）
- src/tools/mod.rs（`pub mod file_mutation_queue` + `pub mod bash` + re-export 新公开项）
- src/lib.rs（核对 `pub use tools::*` 已覆盖 bash 与 FileMutationQueue，未覆盖则补）
- tests/tools.rs（WriteTool/EditTool 构造处改为传 `Arc<FileMutationQueue>`；可加跨 queue 串行化集成测试）
- Cargo.toml（**预期无需改**：tokio `full` 已含 `process`/`time`；若实际缺 feature 再补并说明）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] FileMutationQueue：同一 path 并发 acquire 串行（进入临界区计数，任意时刻 ≤1）；不同 path 并行；guard Drop 后锁可被再次 acquire；acquire 等待可被外层 select 取消
- [ ] WriteTool/EditTool 经 `Arc<FileMutationQueue>` 注入，写 IO 在 guard 持有期间执行；name/description/parameters/resource_scope 与 005 一致（FileWriter/FileWriter）
- [ ] BashTool：`name="bash"`、`resource_scope=Exclusive`；`sh -c "echo hello"` 返回 stdout；非零退出返回 `is_error: true` 且 details 含 `exit_code`；`timeout_ms` 触发时 kill 子进程并返回含 "timeout" 错误；`signal` 取消时 kill 子进程并返回含 "cancelled" 错误
- [ ] 取消/超时后无子进程泄漏（显式 `kill().await` 后 `wait().await` 严格 reap，`kill_on_drop` 兜底）
- [ ] 既有 005 文件工具测试（tests/tools.rs）构造处更新后仍全绿
- [ ] 产品代码无 `unwrap()`；测试内用 `expect("前置条件")`；bash 测试用 `sh -c`（POSIX，不依赖 bash 二进制）
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录
