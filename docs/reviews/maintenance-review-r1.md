# Maintenance Review - Round 1

## 基本信息
- 审查时间: 2026-09-12
- 审查员: guigu-reviewer
- 审查范围: v0.1.0 现有 `src/` 全量回归巡检

## 门禁结果
- cargo check: ✓
- cargo clippy -- -D warnings: ✓
- cargo test: ✓（386 passed, 0 failed）
- cargo fmt --check: ✓

## 代码审查
### 问题
1. [Critical] `src/tools/read.rs:106-112` — offset/limit 按字节计算后直接用于
   `String` 切片，遇到 UTF-8 多字节字符会 panic。
   - 影响: 外部工具调用可以通过例如 `{"path":"...","offset":1}` 触发进程
     panic（若偏移落在中文字符中间）；这与文件头部“字节切片可能截断多字节字符，
     一期接受”的注释不一致，也不是 `ToolError` 可恢复错误。
   - 建议: 在原始 `Vec<u8>` 上完成 offset/limit 切片，再使用
     `String::from_utf8_lossy` 返回文本；或者明确将边界调整到 UTF-8 字符边界并
     增加越界/多字节回归测试。不要用可能 panic 的 `content[start..end]`。

2. [Warning] `src/remote/mod.rs:94-101` — `spawn_stdio` 在已经成功 spawn 并取走
   stdin 后，如果取 stdout 失败，会通过 `?` 返回，未显式 kill/wait 子进程。
   - 影响: 异常配置或 Tokio 管道状态下可能留下未回收的子进程；函数文档承诺避免
     zombie，但该错误路径没有满足承诺。
   - 建议: 将 stdout 获取失败分支改为先 `child.kill().await` 再 `child.wait().await`
     后返回协议错误；或先验证两个管道均可取得，再创建回收 task，并为失败路径补测。

3. [Warning] `src/core/session.rs:369-372、394-404、447-450` — 生产代码对
   `head_committed` 的 `std::sync::Mutex` 使用 `unwrap()`。
   - 影响: 任一持锁线程在临界区 panic 后，后续正常请求会因 mutex poison 再次 panic，
     将可恢复的存储/服务故障升级为进程崩溃；同时违反项目“禁止 unwrap、使用错误处理”
     的约定。
   - 建议: 使用 `unwrap_or_else(|poisoned| poisoned.into_inner())`（若确认状态可继续
     使用），或将 poison 转换为 `SessionError` 并向调用方返回；补充 poison 场景测试。

## 建议
1. `src/tools/read.rs` 的现有测试没有覆盖非 ASCII offset/limit，建议增加中文及
   offset 落在字符中间的测试，防止修复回归。
2. 当前门禁全绿，但测试数量和门禁通过不能替代边界输入审查；优先修复上述 Critical
   问题后再标记维护巡检通过。

## 结论
- [ ] 通过
- [x] 打回

## 下一步
- @guigu-worker 请修复问题 1；问题 2、3 建议一并修复并补充对应测试，然后重新请求
  reviewer 巡检。
