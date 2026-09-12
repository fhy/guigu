# Task 038: TASK_BOARD 备注段归档 HISTORY.md

## Background

`docs/TASK_BOARD.md` 已膨胀至 180+ 行，其中「备注」段（约 83 行）是完整变更史，与 conventions File Layout 约定「TASK_BOARD 为纯索引（max 50 lines）」冲突。

## Goal

将 TASK_BOARD「备注」段完整归档到新建 `docs/HISTORY.md`，`TASK_BOARD.md` 还原为纯任务索引（task ID + 标题 + 状态）。

## Design Notes

- 新建 `docs/HISTORY.md`：归档 TASK_BOARD「备注」段全部变更史（002–036，原样迁移，不删改历史记录）；头部注明归档来源与时间，并追加 037/038 的维护收尾记录。
- `TASK_BOARD.md` 精简为纯索引：标题 + 状态说明 + 归档指针 + 全部任务（002–038）的「ID + 标题 + 状态」单行列表，期信息以 `（N期）` 后缀表达，目标 ≤ 50 行。
- 无代码改动、无行为变化、无新增依赖。

## Files

- docs/HISTORY.md（新建）
- docs/TASK_BOARD.md（精简）

## Acceptance Criteria

- [ ] TASK_BOARD.md ≤ 50 行，为纯索引（含 002–038 全部任务 ID/标题/状态）
- [ ] 备注段全部变更史（002–036）原样归档至 HISTORY.md，无丢失
- [ ] 归档指针 `变更史见 docs/HISTORY.md` 存在
- [ ] Markdown 结构完整
