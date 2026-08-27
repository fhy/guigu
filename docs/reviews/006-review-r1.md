# Task 006 Review - Round 1

## 基本信息
- 审查时间: 2026-08-27
- 审查员: guigu-reviewer
- 任务规格: docs/tasks/006-bash-file-mutation-queue.md

## 门禁结果
- cargo check: ✓
- cargo clippy: ✓ (0 warnings)
- cargo test: ✓ (0 failures, 84 total: 23 lib + 10 bash + 1 queue + 14 tools + 36 既有)
- cargo fmt: ✓

## 代码审查

### 逐文件审查

**src/tools/file_mutation_queue.rs (213行)** — ✓
- `normalize()` 用 `std::path::absolute` + fallback，符合规格（43行）
- 惰性建锁：`std::sync::Mutex` + `or_insert_with`，锁操作极短不跨 await，正确
- `FileMutationGuard` 持 `OwnedMutexGuard<()>` + `PhantomData<&'a ()>` 满足 `Send` + 跨 await + 生命周期声明，设计合理
- 4 个单元测试完整覆盖：同 path 串行、不同 path 并行、guard drop 释放、acquire 可取消
- 已知局限（symlink、锁表只增不减）在模块文档中声明，与规格一致

**src/tools/bash.rs (214行)** — ✓
- `BashArgs` 结构与规格 74 行定义一致：`command: String, cwd: Option<String>, timeout_ms: Option<u64>`
- `build_command()`: `sh -c`、`kill_on_drop(true)`、`Stdio::piped()` stdout/stderr，正确
- `kill_and_reap()`: `kill().await` → `wait().await` 严格 reap，与规格 85 行要求一致
- `join_drain()`: 错误信息含 label，`from_utf8_lossy` 处理非 UTF-8，合理
- `assemble_result()`: exit 0 → `ToolResult::text`，非零 → `is_error: true` + details 含 exit_code/stdout/stderr，正确
- 三路 select 实现（176-206行）：`child.wait()` 用 `&mut self` 不移动 child，取消/超时分支胜出时 wait future 已 drop，分支内可再 `kill()+wait()`——这正是规格 79 行强调的 1A 关键点，实现正确
- 无 `unwrap()`，所有错误路径走 `ToolError`

**src/tools/write.rs (202行)** — ✓
- 构造改持 `Arc<FileMutationQueue>`，与 005 原始签名 `WriteTool::new()` 改为 `new(queue)`，正确
- acquire + 二次取消检查模式与规格 56-57 行一致
- `name`/`description`/`parameters`/`resource_scope` 与 005 保持一致（FileWriter），无破坏性变更
- 6 个单元测试构造处更新为传 `Arc<FileMutationQueue>`，全绿

**src/tools/edit.rs (234行)** — ✓
- 构造改持 `Arc<FileMutationQueue>`，acquire 位置在 metadata/read 之前（89行），保证 read-modify-write 全程持锁，正确
- 写回前有额外取消检查（130-134行），比规格多一层保护，合理增强
- `name`/`description`/`parameters`/`resource_scope` 与 005 保持一致（FileWriter），正确
- 6 个单元测试构造处更新，全绿

**src/tools/mod.rs (13行)** — ✓
- `pub mod bash` + `pub mod file_mutation_queue` + re-export `BashTool`/`FileMutationQueue`/`FileMutationGuard`，完整

**tests/bash.rs (211行)** — ✓
- 10 个集成测试覆盖：契约（name/scope/params）、echo、非零退出、超时、取消、预取消、缺参数、cwd
- 所有命令用 `sh -c`（POSIX），不依赖 bash 二进制
- 超时/取消测试用 `Duration::from_secs(2)` 上限断言 kill 及时性，合理

**tests/file_mutation_queue.rs (72行)** — ✓
- 跨 queue 集成测试：两个 WriteTool 共享 `Arc<FileMutationQueue>` 并发写同一路径，验证文件内容为某次完整写（无交错），正确

**tests/tools.rs (360行)** — ✓
- 构造处同步更新为传 `Arc<FileMutationQueue>`，14 个既有测试全绿

### 文件体量
| 文件 | 行数 | 限制 | 状态 |
|------|------|------|------|
| bash.rs | 214 | 400 | ✓ |
| file_mutation_queue.rs | 213 | 400 | ✓ |
| write.rs | 202 | 400 | ✓ |
| edit.rs | 234 | 400 | ✓ |
| tests/tools.rs | 360 | 400 | ✓ |
| tests/bash.rs | 211 | 400 | ✓ |
| tests/file_mutation_queue.rs | 72 | 400 | ✓ |

### 规格符合性逐项核对
| 验收标准 | 状态 |
|---------|------|
| cargo check / clippy / test / fmt 通过 | ✓ |
| FileMutationQueue 同 path 串行、不同 path 并行、guard drop 可再 acquire、acquire 可取消 | ✓ (4 单元测试覆盖) |
| WriteTool/EditTool 经 Arc 注入，写 IO 在 guard 持有期间 | ✓ |
| BashTool name="bash"、Exclusive、echo 返回 stdout、非零 is_error + exit_code | ✓ |
| timeout_ms kill + "timeout"、signal cancel + "cancelled" | ✓ |
| kill().await + wait().await 严格 reap、kill_on_drop 兜底 | ✓ |
| 产品代码无 unwrap() | ✓ (仅 unwrap_or_else 处理 poisoned mutex 和 path::absolute fallback) |
| 公开 API 有 /// 文档注释 | ✓ |
| 单文件 ≤ 400 行 | ✓ |

## 结论
- [x] 通过
- [ ] 打回

## 总结

实现质量很高，严格遵循了规格的所有关键设计决策：
1. 1A（禁用 wait_with_output）的三路 select 实现正确，`child.wait()` 用 `&mut self` 不移动 child 是关键
2. 2A（锁表只增不减 + 不解析 symlink）正确落地并声明为已知局限
3. 3A（kill+wait 严格 reap）在取消和超时路径均正确实现
4. FileMutationQueue 的惰性建锁 + std::sync::Mutex 锁表 + OwnedMutexGuard 设计符合规格建议
5. WriteTool/EditTool 的 acquire + 二次取消检查模式正确
6. 测试覆盖全面，无虚假绿
