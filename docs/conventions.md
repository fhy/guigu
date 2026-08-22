# guigu Collaboration Conventions

## Project Goal

guigu is a lightweight, Rust-native AI Agent runtime. Inspired by pi (Python agent framework), but rebuilt in Rust for performance and safety.

Core design principles:
- **Trait-based abstraction**: Agents, tools, and runtimes are defined as traits
- **Async-first**: Built on tokio for non-blocking execution
- **Minimal dependencies**: Only essential crates, no bloat
- **Embeddable**: Can be used as a library or run as a standalone binary

Reference implementation: pi (Python) — we are not porting it 1:1, but adopting its architectural ideas where they make sense in Rust.

## Architecture

Three agents collaborate in a Matrix group chat. The PM (user) acts as the central coordinator.

| Role | Agent | Owned Directory | Commit Type |
|------|-------|----------------|-------------|
| **Architect** | guigu-planner | docs/ | docs: |
| **Developer** | guigu-worker | src/ tests/ | feat: / fix: / refactor: |
| **Reviewer** | guigu-reviewer | docs/reviews/ | review: |

Override exception: When PM explicitly authorizes, an agent may touch the other's directory. Mark the commit with `override:`.

## File Layout

```
docs/
├── TASK_BOARD.md          # Index: task ID + title + status (max 50 lines)
├── conventions.md         # This file
├── architecture.md        # Overall architecture design
├── reviews/               # Review results (reviewer-owned)
│   └── NNN-review.md
└── tasks/
    ├── 001-agent-trait.md  # Per-task spec
    ├── 002-message-type.md
    └── ...
```

TASK_BOARD.md is an index only — no details. Each task's full spec lives in `docs/tasks/NNN-xxx.md`.

## Communication

**PM sends instructions**: Use `@botname` prefix in main thread to target a specific agent.
**Blockers**: Agent must clearly mark `BLOCKED` in its reply. PM coordinates.

No separate feedback file needed. Matrix threads naturally isolate per-task discussions.

## Agent Coordination Rules

### PM is the coordinator, not the only communicator

- PM 用 `@botname` 前缀在主线程发指令，明确指定目标 agent
- Agent 之间可以**有限度地直接通信**，不需要每次都经过 PM
- PM 不参与 agent 之间的技术细节讨论

### Direct communication allowed

| From | To | When | Example |
|------|----|------|---------|
| Developer | Reviewer | 代码完成后请审查 | `@guigu-reviewer 请审查 Task 001` |
| Reviewer | Planner | 遇到设计疑问时 | `@guigu-planner 001 的规格有歧义，xxx 应该怎么处理？` |
| Reviewer | Developer | 打回后简单修复 | `@guigu-worker Task 001 第3点需要修复` |
| Any agent | PM | BLOCKED 或需要决策时 | `@fhy 需要你决定：xxx vs yyy` |

### Escalate to PM only when

- Agent 之间无法达成一致
- 需要业务决策（非技术决策）
- BLOCKED 状态无法自行解决
- 跨目录修改需要授权

### Strict phase separation

每个任务遵循这个序列，PM 触发第一步：

```
Step 1: PM → @planner   设计规格
Step 2: PM → @developer  实现代码
Step 3: Developer → @reviewer  审查（无需 PM 参与）
Step 4: Reviewer → @developer  打回修复（简单问题）
        Reviewer → @planner    设计疑问（无需 PM 参与）
        Reviewer → @fhy        需要决策时（PM 参与）
```

**Rules:**
- PM 确认规格完成后才启动开发
- 开发完成后 developer 直接请 reviewer 审查
- 简单修复 reviewer 直接通知 developer
- 只有需要决策时才找 PM
- Agent 不得自行跳转到未授权的阶段

### Role drift prevention

| Agent | MUST do | MUST NOT do |
|-------|---------|-------------|
| Planner | Design specs, update TASK_BOARD, answer design questions | Read src/, run cargo, review code |
| Developer | Implement code, run gates, commit, request review | Design specs, review code, write docs |
| Reviewer | Review code, run gates, commit reviews, ask design questions | Design specs, implement code |

## Daily Workflow

### PM Issues a Task

```
@guigu-planner Please design [module name]
- Requirement: what to do, why
- Output: docs/tasks/NNN-xxx.md
```

### Architect Workflow

1. Read `docs/TASK_BOARD.md` for global context
2. Design the module, write spec to `docs/tasks/NNN-xxx.md`
3. Update `docs/TASK_BOARD.md` index (add one line)
4. Reply: spec is ready for development

### Developer Workflow

1. PM provides task ID
2. Read `docs/tasks/NNN-xxx.md` for the spec
3. Implement code, run DoD gates
4. Reply format:

```
[Done] Task NNN: title
- Changes: list files and why
- Gates: cargo check ✓ / cargo clippy ✓ / cargo test ✓
- Notes: things to watch out for
```

### Reviewer Workflow

1. PM provides task ID
2. Read `docs/tasks/NNN-xxx.md` for the spec
3. Run DoD gates + code review
4. Reply format:

```
[Review] Task NNN: PASS/REJECT
- cargo clippy: ✓ / N warnings
- cargo test: ✓ / N failures
- cargo fmt: ✓ / not formatted
- Issues:
  1. src/xxx.rs:42 — description → suggested fix
```

### Fix After Rejection

Developer, upon receiving rejection:

```
[Fix] Task NNN: fixed
- Reviewer said: xxx
- I changed: yyy
```

Reviewer re-reviews and marks PASS when satisfied.

## Task Spec Template

`docs/tasks/NNN-xxx.md`:

```markdown
# Task NNN: title

## Background
Why this needs to be done

## Goal
What to build

## Design Notes
Key design decisions

## Files
- src/xxx.rs
- src/yyy.rs

## Acceptance Criteria
- [ ] cargo check passes
- [ ] cargo clippy -D warnings passes
- [ ] cargo test passes
- [ ] cargo fmt --check passes
```

## DoD Gates (4 gates)

Every commit must pass all four before pushing:

| # | Gate | Command |
|---|------|---------|
| ① | Compile | `cargo check` |
| ② | Lint | `cargo clippy -- -D warnings` |
| ③ | Test | `cargo test` |
| ④ | Format | `cargo fmt --check` |

If any gate is red, do not commit. Fix and re-run.

## Git Discipline

**Developer:**
- Only `git add src/` `tests/`
- No blanket `git add .`
- One commit per task, code + tests together

**Architect:**
- Only `git add docs/`
- Never touch `src/` `tests/`

**Reviewer:**
- Does not commit code
- Can commit review results to `docs/reviews/`
- Review results also posted in the group chat

**General:**
- No `--no-verify`
- No `git reset` that drops others' commits
- Commit message format: `<type>(<scope>): <description>`

## Commit & Push

Every completed unit of work must be committed and pushed before marking done.

**Rules:**
- Run DoD gates (check → clippy → test → fmt) **before** committing
- One commit per logical unit (one task, one fix, one design doc)
- Commit message follows `<type>(<scope>): <description>` format
- **Pull before pushing** — if remote has new commits, rebase first (`git pull --rebase`), then push
- Push immediately after committing — do not batch multiple commits before pushing
- If push fails due to conflict, rebase on latest main, resolve conflicts, then push
- If rebase is complex (many conflicts), stop and report BLOCKED to PM
- Never force-push (`--force`) unless PM explicitly authorizes

**Who commits what:**

| Role | Commits | Pushes |
|------|---------|--------|
| Developer | `src/` `tests/` | Yes |
| Architect | `docs/` | Yes |
| Reviewer | `docs/reviews/` | Yes |

## Coding Standards

- Rust edition 2024
- Error handling: use `thiserror`, no `unwrap()`
- Public APIs require `///` doc comments
- Unit tests in `#[cfg(test)] mod tests`
- Integration tests in `tests/`
- Modules abstract through traits
- Naming: snake_case functions, PascalCase types, SCREAMING_SNAKE_CASE constants
- Dependencies managed only through Cargo.toml

## Size Limits

| Object | Limit | When exceeded |
|--------|-------|---------------|
| Single file | 400 lines | Split into sub-modules by responsibility |
| Single function | 80 lines | Extract helper, main function only orchestrates |
| Single struct/enum | 200 lines | Split or compose via traits |
| Single test file | 30 `#[test]`s | Split by scenario |

Existing files are not forced to refactor, but AI must split before making large edits to 400+ line files.

## Test Quality

Tests must execute real logic. Fake green is forbidden:

- Compile-pass ≠ test-pass. Must actually run via `cargo test`.
- Tests must use `assert!` / `assert_eq!` or similar. No empty function tests.
- No testing compilation itself (`#[test] fn it_compiles()` is invalid).
- Async logic must use `tokio::test` for real execution, not over-mocked.
- Test files must not hardcode paths or depend on external services.

Mandatory gate tests (cannot delete or weaken):

- `cargo check` compiles
- `cargo clippy -- -D warnings` zero warnings
- `cargo test` all green
- `cargo fmt --check` consistent formatting
