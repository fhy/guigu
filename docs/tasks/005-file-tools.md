# Task 005: 内置文件工具 read / write / edit

## Background

001–004 已交付消息/事件、AgentHandle 生命周期、Runtime 主循环与最小 Echo 端到端。当前 `src/tools/` 仅有一个无副作用的 EchoTool，尚无可落地的真实能力。本任务实现第一批内置工具（文件读/写/编辑），复用 003 定稿的 `Tool` trait 与 004 已建立的 `src/tools/` 结构，让 Runtime 能编排真实文件操作。

`ResourceScope::FileWriter` 的**单 agent 内**写串行化已由 003 主循环编排保证；**跨 agent 同文件**写串行化（file_mutation_queue）属 006，本任务不涉及。

## Goal

- 实现 `ReadTool`（ReadOnly）、`WriteTool`（FileWriter）、`EditTool`（FileWriter）三个 `Tool` 实现
- 每个工具：参数宽松校验、真实文件 IO、错误不 throw（编码进 `ToolError`）
- 单元测试 + 集成测试（以 `Arc<dyn Tool>` 走完整 trait 契约）

## Design Notes

### 契约复用（以既有定稿为准）

- `Tool` trait 完整签名（003 定稿，勿改）：
  `execute(&self, tool_call_id: &str, args: serde_json::Value, signal: CancellationToken, on_update: Option<&dyn Fn(ToolResult)>) -> Result<ToolResult, ToolError>`
- `ResourceScope::{ ReadOnly, FileWriter, Exclusive }`（003 定稿，core/tool.rs）
- `ToolResult { content, details, is_error }`、`ToolError`（thiserror，单 message 结构）定于 `core/tool.rs`（003 定稿）
- `ToolResultContent` 定于 `core/message.rs`，内容段为 struct variant `Text { text: String }`（002 review 认可形态），构造时以 002 定稿为准
- 工具落 `src/tools/`（004 已建 `mod.rs` + `echo.rs`），经 `AgentRuntime { tools: Vec<Arc<dyn Tool>> }` 注册（003 定稿），本任务不新增注册字段

### ReadTool

- `name = "read"`，`resource_scope = ReadOnly`
- `parameters()` 返回宽松 JSON schema（供 LLM/人工参考，不做运行时强校验）：
  ```json
  { "type": "object",
    "properties": {
      "path":   { "type": "string" },
      "offset": { "type": "integer", "minimum": 0 },
      "limit":  { "type": "integer", "minimum": 1 } },
    "required": ["path"] }
  ```
- args：`ReadArgs { path: String, offset: Option<u64>, limit: Option<u64> }`
- 行为：
  1. 入口 `signal.is_cancelled()` 已取消 → 返回取消语义 `ToolError`（不执行 IO）
  2. `serde_json::from_value::<ReadArgs>()` 失败 → `ToolError::invalid_arguments(...)`
  3. `tokio::fs::metadata(path)`：不存在 / 非普通文件 → IO 语义 `ToolError`
  4. `tokio::fs::read(path)` → `String::from_utf8`：非法 UTF-8 → IO 语义 `ToolError`
  5. `offset`/`limit` 按**字节**切片（`offset` 缺省 0，`limit` 缺省读全文）；字节切片可能截断多字节字符，一期接受并在 `details` 记录
  6. 成功 → `Ok(ToolResult { content: vec![Text { text }], details: Some(json!({"path": ..., "bytes": n})), is_error: false })`

### WriteTool

- `name = "write"`，`resource_scope = FileWriter`
- `parameters()`：
  ```json
  { "type": "object",
    "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
    "required": ["path", "content"] }
  ```
- args：`WriteArgs { path: String, content: String }`
- 行为：
  1. 入口取消检查（同 ReadTool）
  2. 反序列化失败 → `invalid_arguments`
  3. 父目录不存在 → `tokio::fs::create_dir_all(parent)`（失败 → IO 语义 `ToolError`）
  4. `tokio::fs::write(path, content.as_bytes())`（覆盖写；原子写走 006）
  5. 成功 → `details: Some(json!({"path": ..., "bytes": content.len()}))`，content 返回简短确认文本（如 `wrote N bytes to <path>`）

### EditTool

- `name = "edit"`，`resource_scope = FileWriter`
- `parameters()`：
  ```json
  { "type": "object",
    "properties": {
      "path": { "type": "string" },
      "old_string": { "type": "string" },
      "new_string": { "type": "string" } },
    "required": ["path", "old_string", "new_string"] }
  ```
- args：`EditArgs { path: String, old_string: String, new_string: String }`
- 行为：
  1. 入口取消检查（同 ReadTool）
  2. 反序列化失败 → `invalid_arguments`
  3. 读文件全文（UTF-8，同 ReadTool 的 IO/UTF-8 错误）
  4. 统计 `old_string` 出现次数：`0` → IO 语义错误 "old_string not found"；`>1` → 错误 "old_string not unique (N matches)"
  5. 唯一匹配 → 替换该处；**写回前**再查一次 `signal.is_cancelled()`，避免取消后仍写
  6. 写回（`tokio::fs::write`）；成功 → `details: Some(json!({"path": ..., "replaced": 1}))`，content 返回简短确认

### 错误语义统一

- 参数校验失败 → `ToolError::invalid_arguments(msg)`（003 已提供）
- 文件 IO / 路径 / UTF-8 / 匹配失败 → IO 语义 `ToolError`
- 若 003 的 `ToolError` 仅提供 `invalid_arguments` 构造器，Developer 在 `core/tool.rs` 补一个 message 构造器或 `io(msg)` 辅助构造器（保持 thiserror + 单 message 结构不变，不改既有变体语义）
- 取消 → 取消语义 `ToolError`（消息含 "cancelled"），由 003 主循环统一产出 `stop_reason: Aborted`，工具本身不直接改 stop_reason

## Files

- src/tools/read.rs（ReadTool + ReadArgs + 单元测试）
- src/tools/write.rs（WriteTool + WriteArgs + 单元测试）
- src/tools/edit.rs（EditTool + EditArgs + 单元测试）
- src/tools/mod.rs（`pub mod read/write/edit` + re-export 三个工具）
- src/lib.rs（核对 004 的 `pub use tools::*` 是否已覆盖新增项，若未覆盖则补 re-export）
- src/core/tool.rs（仅当需补 `ToolError` 的 io/取消构造器时）
- tests/tools.rs（集成测试：以 `Arc<dyn Tool>` 走 name/description/parameters/resource_scope/execute 完整契约 + 真实文件 IO）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo fmt --check passes
- [ ] 三个工具 `name`/`description`/`parameters`/`resource_scope` 与上文一致（ReadOnly / FileWriter / FileWriter）
- [ ] read：读存在文件返回内容；不存在 → IO 错误；`offset`/`limit` 生效；非法 UTF-8 → 错误
- [ ] write：新建文件成功；覆盖写成功；父目录不存在时自动创建
- [ ] edit：唯一匹配替换成功；`old_string` 不存在 → 错误；多处匹配 → 错误
- [ ] 参数缺失/类型不符 → `invalid_arguments`
- [ ] `signal` 已取消 → 不执行 IO 并返回取消语义错误（可用 `CancellationToken::new().cancel()` 先行触发验证）
- [ ] 测试用 `std::env::temp_dir()` + 唯一后缀（进程 id / 计数器），不用硬编码路径，测试结束清理临时文件
- [ ] 产品代码无 `unwrap()`；测试内用 `expect("前置条件")`
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录
