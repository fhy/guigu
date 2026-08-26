# TASK_BOARD.md

状态：[ ] 待做 / [~] 进行中 / [x] 完成

## Backlog

- [x] 002 — Message/Event 数据结构（基础，先行）
- [x] 001 — Agent trait + 生命周期 AgentHandle（依赖 002）
- [x] 003 — Tool trait + Runtime 执行引擎（依赖 001、002）
- [ ] 004 — 最小 Echo Agent 端到端（依赖 003）

## 备注

- 实施顺序：002 → 001 → 003 → 004
- 规格见 docs/tasks/NNN-xxx.md；架构定稿见 docs/architecture.md（v1.0）
- 001 规格 v1.1（2026-08-25）：定稿并发排队、事件序列、wait_for_idle 同步点+超时、reset/abort/shutdown 契约（依据 r6 审查）
- 001 已于 r7 审查通过（2026-08-26，docs/reviews/001-review-r7.md）：四门禁全绿、r6 十项阻塞全部核销
- 003 已于 r2 审查通过（2026-08-26，docs/reviews/003-review-r2.md）：四门禁全绿、r1 五项问题全部核销（含 no-Done→Error 新增测试）
- 二期（暂不排期）：Session 树/JSONL 崩溃恢复、上下文摘要压缩、内置工具集(read/write/edit/bash)、adapters(OpenAI/Anthropic)、远程协议
