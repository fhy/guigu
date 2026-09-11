# Task 036: 架构文档插件 stale 措辞同步

## Background
029 已交付 Agent 插件（`LifecycleHooks` + `AgentFactory` + `AgentPluginRegistry`），architecture.md 中「插件机制」边界排除仍写着「Agent 插件、生命周期钩子」（§7.2 关键决策），已 stale；roadmap.md 候选清单状态亦需同步为「已交付」。

## Goal
订正 architecture.md §7.2 及 roadmap.md 中插件相关 stale 措辞，与 029 已交付状态一致。

## Design Notes
- architecture.md §7.2「插件机制（四期 016）」边界排除项：将「Agent 插件、生命周期钩子」从「边界排除」中移除/改为「已由 029 交付」，保留「动态库 dlopen、跨进程加载」为真正边界。
- roadmap.md：候选清单第 4 条「Agent 插件 / 生命周期钩子」状态同步为「已交付 029」；文首「候选方向清单（待 PM 定序，未立项）」陈旧状态一并订正（当前所有候选已全部交付）。
- 纯文档订正，零代码；与 v0.1.0 终态文档风格一致。

## Files
- docs/architecture.md（§7.2 措辞订正）
- docs/roadmap.md（候选清单状态同步）

## 错误处理
无。

## 测试要求
无（纯文档）。

## Acceptance Criteria
- [ ] 文档措辞与 029 已交付状态一致，无「插件延后/未立项」等 stale 描述
