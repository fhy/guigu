# TASK_BOARD.md

状态：[ ] 待做 / [~] 进行中 / [x] 完成

## Backlog

- [x] 002 — Message/Event 数据结构（基础，先行）
- [~] 001 — Agent trait + 生命周期 AgentHandle（依赖 002）
- [ ] 003 — Tool trait + Runtime 执行引擎（依赖 001、002）
- [ ] 004 — 最小 Echo Agent 端到端（依赖 003）

## 备注

- 实施顺序：002 → 001 → 003 → 004
- 规格见 docs/tasks/NNN-xxx.md；架构定稿见 docs/architecture.md（v1.0）
- 二期（暂不排期）：Session 树/JSONL 崩溃恢复、上下文摘要压缩、内置工具集(read/write/edit/bash)、adapters(OpenAI/Anthropic)、远程协议
