# Task 027 Review - Round 1

## 基本信息

- 审查时间: 2026-09-11
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/027-schemars-tool-params.md
- 审查提交: 673e015

## 门禁结果

- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（326 个库测试、18 个 CLI 测试及全部集成测试通过；schema 集成测试 6 个）
- cargo test --no-default-features: ✓（222 个库测试及全部适用集成测试通过）
- cargo clippy --no-default-features --all-targets -- -D warnings: ✓
- cargo fmt --check: ✓

## 代码审查

### 问题

无阻塞问题。

实现符合 Task 027 规格：

1. `src/core/schema.rs:14-30` 提供了 `schema_for`、`parameters`、`root_schema` 三个 feature-gated helper；序列化/反序列化失败均返回 `Option`，没有产品代码 `unwrap()` 或 panic 路径。
2. `src/tools/{read,write,edit,bash}.rs` 的参数结构体使用 `cfg_attr` 条件 derive，在 `schema` 开关下由 Rust 类型生成 schema；必填字段和 `minimum` 约束与 005/006 手工 JSON 语义一致。
3. `tests/schema.rs:45-103` 对四个工具逐项检查 object 类型、属性键集合、required 集合和数值约束，并验证 Value 与 RootSchema 往返；覆盖了规格中的核心验收条件。
4. `schema` 已加入默认 feature，同时 `--no-default-features` 的编译、测试和 clippy 均通过，说明可选依赖剥离路径有效；`Tool` trait 签名未改变。
5. 新增公开 helper 均有文档注释，改动文件规模符合约定，未越界修改 ACP/插件 wire schema。

### 建议

1. `src/tools/{read,write,edit,bash}.rs` 的 `parameters()` 都重复了同样的 `#[cfg]` 分支。当前实现清晰且符合规格，后续若继续迁移更多内置工具，可考虑统一一个 feature-gated 内部入口，减少重复代码；不作为本任务阻塞项。
2. `src/core/schema.rs:29-30` 当前 `root_schema` 对所有能被 `RootSchema` 反序列化的 JSON 都返回 `Some`，它是结构反序列化而非完整 JSON Schema 校验器。当前规格只要求 Value 往返和非法输入容错，行为符合范围；后续若上层需要严格校验，应另行明确 schema 校验语义，不要复用此 helper 充当 validator。

## 结论

- [x] 通过
- [ ] 打回

## 下一步

- Task 027 无需修复。
- 后续消费者可通过 `root_schema(tool.parameters().as_ref())` 获取类型化 schema；若要暴露校验/表单能力，另立任务处理完整 JSON Schema 校验语义。
