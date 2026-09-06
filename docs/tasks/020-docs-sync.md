# Task 020: 架构文档同步到 v0.1.0 终态

## Background

`docs/architecture.md` 定稿于一期（标题 v1.0，日期占位「2025-XX」未填），其 §2 目录结构、§6 里程碑仅覆盖一期（001–004）。二期~五期（005–018）已交付大量新模块（adapters、session 树、remote、server、acp、plugin、bin/guigu 子模块化），但架构文档未同步，与实现/`TASK_BOARD.md` 不一致。发布 v0.1.0（019）要求「文档与实现一致」。

## Goal

将 `docs/architecture.md` 同步到 v0.1.0 终态，并新增 `docs/roadmap.md` 记录下一阶段路线图。纯文档修订，零代码变化。

## Design Notes

### architecture.md 更新点（保持设计决策不动，仅同步现状）

1. **日期**：标题下「2025-XX」填实际日期（2026-09）。
2. **§2 目录结构**：补全已交付的顶层/子模块，与 `src/` 实际布局一致：
   - `core/`（message/event/agent/agent_runtime/tool/provider/context/compactor/session{runtime+jsonl}）
   - `tools/`（read/write/edit/bash/echo/deferred/file_mutation_queue）
   - `adapters/`（openai/anthropic + acc/sse/stream）
   - `remote/`（protocol/codec/server/client）
   - `server/`（protocol/transport/lane/lane_ops/lane_recovery）
   - `acp/`（jsonrpc/types/mapping/handlers/transport/fs_tool/stdio_client）
   - `plugin/`（mod/tool）
   - `bin/guigu/`（main/cli/repl/acp/assemble/fake/error）
   - 标注各模块对应任务号（005/006/007/009/010/011/012/013/014/015/016）。
3. **§6 里程碑**：一期表保留，追加二期~五期里程碑表（005–018 摘要，可引用 `TASK_BOARD.md` 作为权威索引，避免重复膨胀）。
4. **§7 三期规划**：状态由「规划」改为「已交付」，并补四期（016/017）、五期（018）一句收尾。
5. **一致性**：与 `TASK_BOARD.md` 状态（002–018 全 [x]）一致，无 stale 描述（如「二期」「延后」等已落地措辞需订正）。

### roadmap.md 新增（下一阶段路线图）

内容为候选方向清单（待 PM 定序），每条含：动机（引用遗留来源）+ 大致范围 + 新依赖/风险：

1. **ACP SSE/HTTP 远程多 client** — 014 将 `acp-sse` 降级为存根（`serve_sse`），补齐后支撑编辑器远程多 client。需 axum/reqwest（新依赖，PM 拍板）。
2. **持久化 lane head / 活动分支元数据** — 015 r2 遗留：当前以「最大 NodeId」推断活动叶，多 lane fork 场景无法表达真正活动 lane。需持久化 lane head 或恢复 API 显式收目标 head（真实功能缺口）。
3. **schemars 强类型工具参数** — 架构 §3.4「一期宽松，二期 schemars」预留：`parameters() -> Option<serde_json::Value>` 升级为可生成 JSON Schema 的类型化契约。
4. **Agent 插件 / 生命周期钩子** — 016 排除项：插件机制从 Tool 层扩展到 Agent 层（自定义 agent 类型、钩子注入）。
5. **跨进程会话锁 / 多写者文件锁** — 006/012 声明边界：真实分布式多进程同 session 并发写需文件锁层级。

### 错误处理

无代码，无错误路径。仅文档措辞一致性需人工核对。

### 边界声明（明确不做）

- 不改 `src/` `tests/` `Cargo.toml`。
- 不改 `TASK_BOARD.md` 已完成项的历史备注（除 019/020 索引行）。
- 不删历史审查报告（`docs/reviews/`）与任务规格（`docs/tasks/`）。

## Files

- docs/architecture.md（同步终态）
- docs/roadmap.md（新增，下一阶段路线图）

## Acceptance Criteria

- [ ] architecture.md 日期已填（2026-09），无「2025-XX」占位
- [ ] §2 目录结构覆盖 `src/` 全部顶层模块（core/tools/adapters/remote/server/acp/plugin/bin），并标注对应任务号
- [ ] §6 里程碑补全二期~五期（005–018），或引用 TASK_BOARD 作为权威索引且无遗漏
- [ ] §7 三期状态改为「已交付」，四期/五期有收尾说明
- [ ] 全文无 stale 措辞（「延后」「二期」「预留」等已落地项已订正）
- [ ] docs/roadmap.md 新增，含上述 5 条候选方向（动机 + 范围 + 依赖/风险）
- [ ] 与 TASK_BOARD.md 状态一致（002–018 全 [x]）

## 修订记录

- v1.0（2026-09-06，Architect）：初稿。architecture.md 同步 v0.1.0 终态（填日期/补目录/补里程碑/订正状态）+ 新增 roadmap.md 记录 5 条下一阶段候选方向。纯文档，零代码。
