# Task 031: schemars 工具参数统一入口 + root_schema 语义澄清

## Background
027 r1 非阻塞建议 1 & 2（docs/reviews/027-review-r1.md）：
1. `src/tools/{read,write,edit,bash}.rs` 的 `parameters()` 各自重复 `#[cfg]` 分支，可提炼统一 feature-gated 内部入口减少重复。
2. `src/core/schema.rs:29-30` 的 `root_schema` 当前对任何能被 `RootSchema` 反序列化的 JSON 都返回 `Some`，本质是「结构反序列化 / Value→RootSchema 往返」而非「完整 JSON Schema 校验器」，需在文档注释中澄清语义，避免被上层误当 validator。

## Goal
（1）提炼统一 feature-gated helper，收敛四个内置工具 `parameters()` 的 `#[cfg]` 重复分支；（2）澄清 `root_schema` 语义（文档注释，必要时更名）。

## Design Notes
- 统一入口：在 `src/core/schema.rs`（或合适位置）提供 feature-gated 内部 helper（如返回 `Option<RootSchema>` 的构建函数），四个工具的 `parameters()` 委托该 helper；`Tool::parameters()` 对外签名不变。
- `root_schema` 语义澄清：文档注释明确「仅为结构反序列化 / Value→RootSchema 往返，不做完整 JSON Schema 校验」；是否更名以实际命名冲突为准（无冲突可保留原名仅补注释，避免无谓 breaking）。
- 零破坏：不改变 `Tool` trait、不改变任何工具对外 schema 内容（object 类型 / required 集合 / 数值约束与 005/006 手工 JSON 语义一致）。

## Files
- src/core/schema.rs（统一 helper + root_schema 文档注释）
- src/tools/{read,write,edit,bash}.rs（parameters() 委托统一入口）

## 错误处理
无新错误类型；helper 保持既有「反序列化失败返回 None」容错语义。

## 测试要求
- 既有 `tests/schema.rs` 四工具逐项校验（object 类型 / 属性键 / required / 数值约束 / Value↔RootSchema 往返）全部保持通过。
- 可选：新增统一 helper 的直接单测。

## Acceptance Criteria
- [ ] cargo check
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] cargo test --all-targets
- [ ] cargo test --no-default-features
- [ ] cargo fmt --check
