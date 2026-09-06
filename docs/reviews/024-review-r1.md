# Task 024 Review - Round 1

## 基本信息

- 审查时间: 2026-09-07
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/024-lane-head-persistence.md
- 审查提交: 1cc7ca9

## 门禁结果

- cargo check: ✓
- cargo clippy --all-targets -- -D warnings: ✓
- cargo test --all-targets: ✓（283 库测试、10 个 CLI binary 测试及全部集成测试通过）
- cargo fmt --check: ✓

## 代码审查

### 问题

1. **[Critical] message 与 lane head 不是同一写入临界区，恢复状态可能不一致**
   - 位置：`src/core/session/lane_writer.rs:89-97`、`src/core/session.rs:330-363`
   - 影响：`LaneWriter::append` 先通过 `SharedSessionStorage::append` 获取并释放 message 写锁，再单独调用 `append_lane_head`。后者直接委托底层 store，绕过同一把 `write_lock`。因此 message 行和 head 行之间可被其它 lane/写者插入，且二者不是原子提交单元；并发或写入失败后，重放得到的 head 可能不是该 lane 最后一次成功 append 的 head。
   - 建议：在 `SharedSessionStorage` 增加组合提交操作，在同一写锁内完成 message 与对应 `LaneHeadRecord` 的追加；或将 lane-head 写入纳入同一协调入口。不能仅分别给两个操作加锁。

2. **[Critical] head 持久化失败后，内存 head 已推进但 append 返回错误**
   - 位置：`src/core/session/lane_writer.rs:89-97`
   - 影响：message 已落盘、`self.head` 已更新，而 head record 可能未落盘；后续 bridge 会继续使用已推进的内存 head，重启后却只能看到旧持久化 head，导致 transcript/分支恢复错误。
   - 建议：采用统一提交单元；若 head 写失败，必须阻止该 lane 继续写并明确返回/传播失败，或实现可恢复的 pending/补偿协议。不能把该错误当作普通 append 错误后继续沿用新 head。

3. **[Critical] spawn/fork 在 lane 注册前持久化初始 head，失败请求可污染正式 lane**
   - 位置：`src/server/lane.rs:77-101`、`src/server/lane.rs:153-182`
   - 影响：并发请求先通过“lane 不存在”检查，随后在最终登记前写入 append-only head 记录；失败请求在二次校验发现 `LaneAlreadyExists` 或 session/lane 状态变化后虽清理 runtime，却无法撤销已写记录。重复 spawn 甚至可能写入 `lane -> None` 覆盖已有 lane 的真实 head；fork 也可能残留错误分叉点，影响重启恢复。
   - 建议：在 session 锁内先原子预留 lane ID，只有预留成功者才能初始化并持久化；或引入带 generation/事务状态的提交协议，确保未成功登记的请求不会成为恢复时的最终 head。

4. **[High] 初始 head 持久化失败仅记录 warning，spawn/fork 仍返回成功**
   - 位置：`src/server/lane.rs:98-101`、`src/server/lane.rs:179-182`
   - 影响：空 spawn lane 或尚未 append 的 fork lane 在日志中没有可区分的初始 head，恢复时会触发“无记录则回退最大 NodeId 叶”，可能恢复到其它 lane 的分支；调用方同时被错误告知 lane 创建成功。
   - 建议：持久化失败时清理新建 runtime/bridge、不登记 lane，并返回 `ServerError::Session`；或者实现可靠重试/补偿并明确状态，不能静默降级。

5. **[High] `StorageFactory` 公共 API 发生 breaking change，违背 017-a 已确定的兼容性契约**
   - 位置：`src/server/mod.rs:56-62`、`src/server/mod.rs:161-170`
   - 影响：原 `StorageFactory`/`with_storage_factory` 接受 `Arc<dyn SessionStorage>`，现在改为返回 `SessionStorageBundle`。已有嵌入方的闭包将在编译期失败；017-a 明确要求 `StorageFactory`、`with_storage_factory`、`create_session`、`load_session` 的公共签名保持不变。
   - 建议：恢复旧 `StorageFactory` 与 `with_storage_factory` 签名并将其适配为无 head store；另增 `StorageBundleFactory`/`with_storage_bundle_factory` 或等价扩展 API，不要替换既有公开契约。补充旧调用方式的编译/API 回归测试。

6. **[Medium] `SessionStorageBundle` 未约束 message storage 与 head store 的一致性**
   - 位置：`src/server/mod.rs:43-54`
   - 影响：公开字段允许把两个不同 session/文件的后端任意组合，恢复时可能读取不属于当前树的 head，产生错误或不可诊断的 `Protocol` 失败。
   - 建议：优先让同一具体后端统一提供两种能力并由构造函数绑定；若保留可分离字段，至少增加 session identity/一致性校验并在文档中明确约束。

7. **[Medium] 恢复分别读取 tree 与 lane heads，没有一致性快照或恢复锁**
   - 位置：`src/server/lane_recovery.rs:60-68`、`src/server/lane_recovery.rs:108-116`
   - 影响：`load()` 与 `load_lane_heads()` 之间若发生 append，可把旧 tree 与新 head 拼接；新 head 可能不在已读取的 tree 中，恢复结果错误。当前实现依赖调用方保证没有并发写入，但状态机/API 未强制该前提。
   - 建议：恢复期间阻止该 session 的写入，或提供单次一致性快照/恢复锁，一次解析同一日志视图。

### 测试缺口

现有测试覆盖了 roundtrip、半行、旧 Message-only 文件和顺序多 lane fork，但未覆盖以下验收关键路径：空 lane 初始 `None` 恢复、fork 后尚未 append 的恢复、head store 写失败、message/head 并发交错、重复 spawn/fork 竞态，以及旧 `StorageFactory` API 兼容性。建议在修复上述问题后补充回归测试，尤其断言每个 lane 恢复出的 head 等于该 lane 最后一次成功 append。

## 结论

- [ ] 通过
- [x] 打回

## 下一步

- @guigu-worker 请修复问题 1-5；问题 6-7 需同步收敛一致性边界。
- 修复后重新运行四门禁，并补充并发、失败恢复、空 lane/fork 以及公共 API 兼容性测试，再提交下一轮审查。
