# Task 037: 架构文档 stale 引用收尾

## Background

v0.1.0 收尾冻结后复查发现 `docs/architecture.md` 存在 5 处 stale 引用：027（schemars）、025（ACP SSE/HTTP）、028（跨进程锁）均已交付并通过审查，但正文仍写「见 roadmap」「存根」「不在范围」，与交付事实冲突。

## Goal

订正 `architecture.md` 的 5 处 stale 引用，使其与 027/025/028 交付事实一致。零代码、纯文档收尾。

## Design Notes

五处订正（信息源：`TASK_BOARD.md` / `roadmap.md` / 027/025/028 规格）：

1. **§3.4 `Tool` trait `parameters()` 注释**（现写「schemars 强类型化见 roadmap」）→ 订正为 027 已交付：`schema` feature（default 开启），Value↔RootSchema 往返，零破坏 `Tool` trait。
2. **§5「刻意不引入」清单**（现含「schemars/jsonschema（后续，见 roadmap）」）→ 移除该条；schemars 已由 027 引入。
3. **§7.1 分层图**（现写「SSE 为存根（见 roadmap）」）→ 订正为 025 已交付 SSE/HTTP 远程多 client。
4. **§7.2 ACP 决策**（现写「`serve_sse` 为存根（014），补齐见 roadmap 候选 1」）→ 订正为 025 已交付；并修正编号错误（候选 1 为 024 lane head，SSE/HTTP 是候选 2）。
5. **§7.2 多 lane 决策**（现写「跨进程多写者（文件锁）仍不在范围」）→ 订正为 028 已交付 `fs2` 文件锁（可选叠加，零破坏默认行为）。

补充：§5 技术选型表补 `schemars` 一行（027，feature `schema` default）。

## Files

- docs/architecture.md

## Acceptance Criteria

- [ ] 5 处 stale 引用全部订正，与 027/025/028 交付事实一致
- [ ] 无残留指向 027/025/028 的「见 roadmap」「存根」「不在范围」stale 措辞
- [ ] §5 技术选型表补 schemars 行
- [ ] Markdown 结构完整（表格/代码块/列表闭合）
