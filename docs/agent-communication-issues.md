# Agent 间通信问题与解决方案

## 概述

在部署 3 个 Matrix bot（Worker、Reviewer、Planner）协作开发 Rust 项目过程中，遇到多个 agent 间通信问题。本文档记录问题现象、根因分析和解决方案。

## 架构

```
PM (@fhy)
  ↓ 指派任务
Guigu Planner (@guigu-planner) — 架构设计
  ↓ 设计规格
Guigu Worker (@guigu-worker) — 代码实现
  ↓ 提交审查
Guigu Reviewer (@guigu-reviewer) — 代码审查
  ↓ 打回/通过
Worker ←→ Reviewer 迭代
```

## 问题列表

### 1. Worker→Reviewer 消息不回复

**现象**
Worker 完成任务后在群里 `@guigu-reviewer`，Reviewer 无任何响应。

**根因分析**
Worker 的 `handleRoomMessage` 路由逻辑（`connectors/matrix.ts:415-440`）：

```typescript
const botNameQuery = extractBotNameQuery(body, BOT_NAME)
if (body.startsWith(TRIGGER + " ")) {
  query = body.slice(TRIGGER.length + 1).trim()
} else if (body.startsWith(TRIGGER)) {
  query = body.slice(TRIGGER.length).trim()
} else if (body.includes(myUserId)) {
  query = body.replace(myUserId, "").trim()
} else if (botNameQuery !== null) {
  query = botNameQuery
} else if (isDM) {
  query = body
} else {
  return  // 跳过消息
}
```

`extractBotNameQuery`（`connectors/matrix-thread-helpers.ts:41-54`）只检查文本**开头**：

```typescript
export function extractBotNameQuery(text: string, botName: string): string | null {
  const name = botName.trim()
  if (!name) return null

  const prefixes = [`@${name}`, name]

  for (const prefix of prefixes) {
    if (text.slice(0, prefix.length).toLowerCase() !== prefix.toLowerCase()) continue
    const separator = text.charAt(prefix.length)
    if (separator !== ":" && !/\s/.test(separator)) continue
    return text.slice(prefix.length).replace(/^[:\s]+/, "").trim()
  }

  return null
}
```

Worker 模型输出的 `@guigu-reviewer` 可能在文本**中间或结尾**，`extractBotNameQuery` 返回 null，消息被跳过。

**修复方案**
在 worker 的路由逻辑中增加 bare mention 检查：

```typescript
} else if (botNameQuery !== null) {
  query = botNameQuery
} else if (body.toLowerCase().includes(`@${BOT_NAME.toLowerCase()}`)) {
  // Bare mention anywhere in text (e.g. from reviewer's model output)
  query = body.replace(new RegExp(`@${BOT_NAME}`, "gi"), "").trim()
} else if (isDM) {
```

**修复效果**
Worker 现在能识别文本中任意位置的 `@guigu-worker` 提及。

---

### 2. Reviewer→Worker 消息不回复

**现象**
Reviewer 打回代码后在群里 `@guigu-worker`，Worker 无响应。

**根因分析**
同问题 1。Reviewer 模型输出 `@guigu-worker` 不在文本开头，Worker 的路由逻辑跳过消息。

**修复方案**
同问题 1。一次修复解决两个方向的通信。

---

### 3. Reviewer 输出格式不遵循

**现象**
Reviewer（使用 `opencode/big-pickle` 模型）完成审查后只输出 100-400 chars 的思考过程，不输出要求的格式：

```
[Review] Task NNN: 通过/打回
- cargo clippy: ✓
- cargo test: ✓
- cargo fmt: ✓
- 问题：
  1. src/xxx.rs:42 — 描述 → 建议修复
```

**根因分析**
`opencode/big-pickle` 模型不遵循 AGENTS.md 的格式要求，将思考过程作为最终输出。

**修复方案**
切换 Reviewer 模型到 `opencode/hy3-free`：

1. 创建项目级 opencode 配置（`/home/fhy/guigu/.opencode/config.json`）：
```json
{
  "$schema": "https://opencode.ai/config.json",
  "model": "opencode/hy3-free"
}
```

2. 重启 Reviewer 服务：
```bash
sudo systemctl restart opencode-bridge-reviewer
```

**修复效果**
- 旧模型（big-pickle）：100-400 chars 输出
- 新模型（hy3-free）：1989 chars 输出，32 个工具调用

---

### 4. Reviewer 不 @Worker

**现象**
Reviewer 完成审查但不 `@guigu-worker`，Worker 无法收到修复指令。

**根因分析**
同问题 3。模型不遵循格式要求，自然不会在输出中包含 `@guigu-worker`。

**修复方案**
同问题 3。切换模型后解决。

---

## 经验教训

### 1. 模型选择很重要
- 不同模型对指令的遵循程度差异很大
- `opencode/big-pickle` 免费但不遵循格式
- `opencode/hy3-free` 免费且更遵循指令

### 2. 路由逻辑要宽松
- Agent 模型输出的 mention 位置不可预测
- 不应假设 mention 在文本开头
- 应支持文本任意位置的 mention 匹配

### 3. 格式要求要明确
- 在 AGENTS.md 中用 `⚠️` 标记关键格式要求
- 明确说"不要输出思考过程作为最终回复"
- 提供完整的输出示例

### 4. 调试日志很重要
- 添加 `[RAW]`、`[SEND]`、`[SEND_REPLY]` 等调试日志
- 记录消息发送和接收的完整内容
- 便于快速定位通信问题

## 相关文件

- `connectors/matrix.ts` — Matrix 连接器，消息路由逻辑
- `connectors/matrix-thread-helpers.ts` — `extractBotNameQuery` 函数
- `config/worker/AGENTS.md` — Worker 角色定义
- `config/reviewer/AGENTS.md` — Reviewer 角色定义
- `/home/fhy/guigu/.opencode/config.json` — 项目级 opencode 配置

## 后续优化

1. **统一 mention 解析**：创建公共函数处理所有 bot 的 mention 匹配
2. **模型回退机制**：如果模型不遵循格式，自动尝试其他模型
3. **消息队列**：使用消息队列确保消息不丢失
4. **健康检查**：定期检查 agent 间通信是否正常
