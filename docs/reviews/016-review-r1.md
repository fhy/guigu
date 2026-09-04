# Task 016 Review - Round 1

## 基本信息
- 审查时间: 2026-09-05
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/016-plugin-registry.md
- 审查提交: e8d6766

## 门禁结果
- cargo check: ✓ (`cargo check --all-targets`)
- cargo clippy: ✓ (`cargo clippy --all-targets -- -D warnings`，零 warning)
- cargo test: ✓ (`cargo test --all-targets`，254 个单元测试及全部集成测试通过)
- cargo fmt: ✓ (`cargo fmt --check`)

注：当前环境的 `cargo` 未加入 PATH，实际使用 `/home/fhy/.cargo/bin/cargo` 执行上述命令，结果不受影响。

## 代码审查
### 问题
无阻断性问题。实现满足规格中的 Plugin trait、异步 OnceCell 惰性实例化、失败重试、确定性组装、注册表操作和 Tool 契约透传要求；产品代码未发现 `unwrap()`，公开 API 也有文档注释。

### 建议
1. `src/plugin/mod.rs:127-134` — `tools()` 在持有 `RwLock` 读锁期间调用外部实现的 `plugin.tools()`。虽然该回调当前是同步的，但它属于插件提供方代码，可能执行较慢，或在回调中访问同一注册表而造成锁重入死锁；同时会阻塞所有写操作。
   - 建议：在锁内仅复制并按 id 排序所需的 `Arc<dyn Plugin>`（或复制 `(id, Arc)`），随后释放读锁，再调用 `tools()` 并创建 `PluginTool`。这样符合“短临界区”设计，也避免把外部回调置于注册表锁内。
2. `src/plugin/tool.rs:29-39` — `PluginTool::new` 对 `spec.name` 与插件实际声明没有运行时一致性校验，公开构造函数可以创建“schema 已声明但 instantiate 不支持”的对象；当前行为最终只能在首次 execute 时返回字符串化的 `ToolError`。
   - 建议：若构造函数必须保持无 fallible API，则明确将该约束记录为调用方责任并增加 debug/test 覆盖；若希望 API 更健壮，可提供 `try_new`，校验 `plugin.tools()` 中存在该名称（注意不要在注册表锁内调用）。此项不阻断本任务，因为规格明确由 `PluginRegistry::tools()` 注入一致数据，且错误路径已有测试。

## 结论
- [x] 通过
- [ ] 打回

## 下一步
- 建议在后续修改 `PluginRegistry::tools()` 时先移出读锁再执行 `Plugin::tools()`；当前无需阻塞 Task 016 合并。
