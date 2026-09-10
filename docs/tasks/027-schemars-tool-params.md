# Task 027: schemars 强类型工具参数

## Background

003 定稿 `Tool::parameters() -> Option<serde_json::Value>`，各内置工具（005/006）以**手工 JSON 字符串**声明参数 schema（如 bash 的 `{"type":"object","properties":...,"required":[...]}`）。这套 JSON 本质上就是 JSON Schema，但存在三点缺陷：

1. **无类型安全**：schema 与 `XxxArgs` 反序列化结构体是两处独立事实源，字段增删时靠人肉对齐，易漂移。
2. **无编译期校验**：手工 JSON 拼错字段名/漏 `required` 只能靠运行时发现。
3. **消费方拿不到类型化 schema**：ACP/插件/编辑器想做参数校验、表单生成，只能解析裸 `Value`，无法得到结构化的 `RootSchema`。

architecture §3.4 已预留「schemars 强类型化见 roadmap」，roadmap 候选 3 立项。本任务引入 `schemars`，让参数 schema 从 Rust 类型 `derive` 生成，替代手工 JSON，并暴露 `RootSchema` 访问给上层。

## Goal

- 引入 `schemars`（derive 宏），feature-gated 为 `schema`（**default 开启**，理由见下）
- 提供 helper：`parameters::<T: JsonSchema>() -> Option<serde_json::Value>`（供 `Tool::parameters` 返回）+ `schema_for::<T>() -> RootSchema` + `root_schema()`（Value → RootSchema 反序列化，供上层消费）
- 内置工具参数结构体（`ReadArgs`/`WriteArgs`/`EditArgs`/`BashArgs`）加 `#[derive(JsonSchema)]`，`parameters()` 改为从类型生成
- **零破坏**：`Tool` trait 签名不变（`parameters()` 仍是 `Option<serde_json::Value>`），旧手工 JSON 仍合法

## Design Notes

### 依赖与 feature（Cargo.toml）

```toml
[dependencies]
schemars = { version = "0.8", optional = true }   # 默认特性，不启用 chrono/uuid/url 等可选特性

[features]
default = ["providers-http", "config", "schema"]  # 终态：追加 "schema"
schema = ["dep:schemars"]
```

**为何 default 开启**：schemars 核心依赖仅为 `serde` + `serde_json` + derive 宏（两者已在 core 依赖中），可选特性（chrono/uuid/url）一律不启用，故 default 增量开销可忽略；而收益是「内置工具 schema 从类型 derive，零手工漂移」且「嵌入方 default-features=false 可剥离」。与 `config`（toml，轻 + 开箱即用）同策略。

### Helper（src/core/schema.rs，feature-gated）

```rust
//! 类型化工具参数 schema 辅助。仅 `schema` feature 下编译。

use schemars::JsonSchema;

/// 从实现了 JsonSchema 的类型生成 JSON Schema（RootSchema）。
/// 薄封装 `schemars::schema_for!`，统一入口。
pub fn schema_for<T: JsonSchema>() -> schemars::schema::RootSchema {
    schemars::schema_for!(T)
}

/// 供 `Tool::parameters()` 使用：从类型 derive 生成 schema 并序列化为 Value。
/// 序列化理论不会失败（RootSchema 字段均 JSON 兼容），失败返回 None（无 unwrap）。
pub fn parameters<T: JsonSchema>() -> Option<serde_json::Value> {
    serde_json::to_value(schema_for::<T>()).ok()
}

/// 从 `Tool::parameters()` 的 Value 反序列化为类型化 RootSchema，供 ACP/插件/编辑器消费。
/// 旧手工 JSON 若不符合 schemars SchemaObject 形状则返回 None，调用方据此降级（容错）。
pub fn root_schema(params: Option<&serde_json::Value>) -> Option<schemars::schema::RootSchema> {
    params.and_then(|v| serde_json::from_value(v.clone()).ok())
}
```

- 三个 helper 全为纯函数，无 I/O、无 async，可单元测试。
- `root_schema` 收 `Option<&Value>`（对齐 `Tool::parameters()` 返回值），失败返回 `None` 不 panic。

### 内置工具迁移（005/006 参数结构体）

以既有 `XxxArgs` 结构体为准，逐个加 `#[derive(JsonSchema)]`（保留既有 `Serialize, Deserialize`），并把 `parameters()` 手工 JSON 替换为 `parameters::<XxxArgs>()`：

| 工具 | 文件 | 参数结构体 | 迁移后 `parameters()` |
|---|---|---|---|
| read | src/tools/read.rs | `ReadArgs { path, offset, limit }` | `parameters::<ReadArgs>()` |
| write | src/tools/write.rs | `WriteArgs { path, content }` | `parameters::<WriteArgs>()` |
| edit | src/tools/edit.rs | `EditArgs { path, old_string, new_string }` | `parameters::<EditArgs>()` |
| bash | src/tools/bash.rs | `BashArgs { command, cwd, timeout_ms }` | `parameters::<BashArgs>()` |
| echo | src/tools/echo.rs | 无参数（`parameters() = None`） | 不变（`None`） |

- derive 生成的 schema 语义须与 005/006 手工 JSON **一致**（字段名、类型、`required`、数值约束）。要点：
  - `required` 由非 `Option` 字段自动生成（`path`/`content`/`command` 等必填项非 Option → 自动进 required）。
  - 数值约束用 serde/schemars 属性对齐：如 bash `timeout_ms` 的 `minimum: 1` 用 `#[schemars(range(min = 1))]`；read 的 `offset` 的 `minimum: 0` 用 `#[schemars(range(min = 0))]`。
  - 字段名默认 snake_case；若需与既有 JSON 键完全一致（如 `old_string`），Rust 字段名天然 snake_case，无需额外 rename（除非既有键名不同）。
- **验收对齐**：迁移后每个工具 `parameters()` 输出的 JSON 与 005/006 规格中的手工 JSON **语义等价**（字段集合、required、约束一致；`$schema`/`title` 等 schemars 附加字段允许存在）。加单测断言关键字段（`type=object`、`properties` 键集合、`required` 集合），不逐字节比对。

### 上层消费（可选加分，非硬性 DoD）

- ACP（014）/插件（016）可经 `root_schema(tool.parameters().as_ref())` 拿到 `RootSchema`，用于参数校验/表单生成。本任务仅交付 helper 与单测，**不强制改 014/016 消费逻辑**（ACP 工具 schema 的 wire 暴露需对照官方 spec，属后续独立任务）。
- 声明：`Tool` trait 不新增方法、不引入 `as_any` downcast；类型化访问统一走 `parameters()` → `root_schema()` 这一条路径，避免「双事实源」。

### 边界声明（明确不做）

- 不改 `Tool` trait 的 5 个方法签名；不引入独立 `TypedParameters` trait / `dyn Any` 探测（Value → RootSchema 往返已足够，避免过度设计）。
- 不在本任务改 ACP `AgentCapabilities` / 插件 `DeferredToolSpec` 以携带 `RootSchema`（属后续，声明为边界）。
- `DeferredToolSpec.parameters`（011）仍为 `Option<serde_json::Value>`，不改；其 schema 生成可由上层复用 `parameters::<T>()` 填入。

## Files

- Cargo.toml（`schemars` optional dep + `schema` feature + default 追加）
- src/core/schema.rs（三个 helper + 单测，`#[cfg(feature = "schema")]`）
- src/core/mod.rs（`#[cfg(feature = "schema")] pub mod schema;`）
- src/lib.rs（`#[cfg(feature = "schema")]` re-export helper；核对 feature 声明）
- src/tools/read.rs / write.rs / edit.rs / bash.rs（参数结构体加 derive + `parameters()` 改类型生成 + 同步单测）
- tests/schema.rs（集成测试：derive 生成 schema 语义对齐 + Value↔RootSchema 往返）

## Acceptance Criteria

- [ ] cargo check passes
- [ ] cargo clippy --all-targets -D warnings passes
- [ ] cargo test --all-targets passes
- [ ] cargo test --no-default-features passes（验证 `schema` 剥离后 core 仍编译，helper 被 cfg 移除）
- [ ] cargo fmt --check passes
- [ ] 四个内置工具 `parameters()` 输出与 005/006 手工 JSON **语义等价**（`type=object`、`properties` 键集合、`required` 集合、数值约束一致），单测逐工具断言
- [ ] `parameters::<T>()` 返回 `Some(Value)` 且可被 `root_schema()` 反序列化回 `RootSchema`（往返一致）
- [ ] `root_schema(None)` 返回 `None`；`root_schema(非法 JSON)` 返回 `None`（容错不 panic）
- [ ] `schema_for::<T>()` 返回的 `RootSchema` 可 `serde_json::to_value` 后再 `from_value` 还原（Serialize/Deserialize 闭环）
- [ ] 产品代码无 `unwrap()`；测试内用 `expect("前置条件")`；纯函数单测无需 tokio
- [ ] 单文件 ≤ 400 行，超则拆子模块并记录

## 修订记录

- v1.0（2026-09，Architect）：初稿。roadmap 候选 3 立项，十期开篇。引入 `schemars`（feature `schema` default 开启，核心依赖仅 serde/serde_json 增量可忽略）；helper 三件套（`schema_for`/`parameters`/`root_schema`）；内置工具 read/write/edit/bash 参数结构体加 `#[derive(JsonSchema)]` + `parameters()` 改类型生成；零破坏 `Tool` trait（不改签名、不引入 downcast，Value→RootSchema 往返替代双事实源）；ACP/插件消费 RootSchema 属后续独立任务，声明为边界。
