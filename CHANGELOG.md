# Changelog

All notable changes to guigu are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-09-22

Publishes the correctness, robustness, and code hygiene improvements delivered in phases 12–16 (040–049).

### Runtime correctness

- Protects `tool_call` transcript integrity: oversized batches fail as a whole without execution and receive synthetic error `ToolResult` entries.
- Adds cancellation and timeout handling for provider streams with `ProviderError::Aborted` / `Timeout` and configurable `LoopConfig::request_timeout`.

### Context safety

- Separates context request projection from transcript mutation through `PreparedContext` and an explicit commit step; failed or cancelled requests leave the authoritative transcript unchanged.
- Truncates at turn/user boundaries so context preparation never creates orphaned `ToolResult` messages.

### Retry and rate limiting

- Classifies provider failures as transient, permanent, or rate-limited for retry decisions.
- Parses both seconds and HTTP-date forms of `Retry-After`.

### Precise context budgeting

- Applies the `context_window - reserve_output_tokens` budget before fixed protocol overhead.
- Uses the latest transcript `AssistantMessage.usage.input` as the estimation baseline with incremental character-based accounting, falling back to full estimation when usage is unavailable.

### Code hygiene

- Consolidates retry-after adapters, aligns context-budget API documentation, splits runtime-loop tests, deduplicates test helpers, removes redundant re-exports, and completes documentation cleanup.

## [0.2.0] - 2026-09-13

Adds the incremental capabilities delivered since v0.1.0 (phases 9–11) to crates.io. All changes are additive — no breaking changes to the public trait contracts.

### Configuration

- Custom model configuration via TOML (feature `config`, default on): `ModelConfig` / `Protocol` / `GuiguConfig` data model, a `ProviderFactory` reusing the built-in adapters, and CLI `--config` / `--base-url` / `--api-key-env` with an extended `-m` (config-name-first, inline fallback) and a four-stage `api_key` resolution chain (CLI > plaintext > env > protocol default).
- Custom system prompt and inline `base_url` endpoint override: `--system-prompt` global flag with the default identity set to Guiguzi, and `--base-url` passthrough to the provider.

### TUI

- Full-screen TUI (feature `tui`, opt-in): a `guigu tui` subcommand with a three-zone layout (status bar / conversation / input) and single-column inline tool cards, driven by pure `apply_event` / `handle_key` logic with headless `TestBackend` rendering.

### Sessions

- Persistent lane head: the active branch pointer is now persisted and recovered across restarts, with a unified read/write lock and generation-validated rollback.

### Remote & protocols

- ACP SSE/HTTP transport for remote multi-client (feature `acp-sse`, opt-in): `axum`-based SSE/HTTP serving with per-session permission mode to prevent cross-client crosstalk.

### Tools

- Strongly-typed tool parameter schemas (feature `schema`, default on): built-in `read` / `write` / `edit` / `bash` parameter structs derive `JsonSchema`, so `Tool::parameters` is generated from types (no manual drift) and a `RootSchema` is exposed for upstream consumers.

### Concurrency & locking

- Cross-process session / file locking: a kernel-level `FileLock` primitive (Unix `flock` / Windows `LockFileEx`, auto-released on crash) with blocking / non-blocking / cancellable acquisition, optionally layered onto the file mutation queue and JSONL session storage.

### Extension

- Agent plugins and lifecycle hooks: a `LifecycleHooks` trait (before/after tool call, should-stop, prepare-next-turn), an `AgentFactory` + `AgentPlugin` + `AgentPluginRegistry`, and optional bridging into the runtime loop config.

### Housekeeping

- Maintenance pass (phases 11): lock-discipline cleanup, removal of `unwrap`/`expect`, unified schema entry point, lock-file update, and CI package-whitelist validation.

## [0.1.0] - 2026-09-06

First release: a complete, embeddable AI Agent runtime.

### Core runtime

- Message/Event data model with serialization round-trip.
- `Agent` trait + `AgentHandle` lifecycle: command queue, snapshot, event subscription, abort/reset/shutdown, `wait_for_idle`.
- `Tool` trait + single-writer runtime loop with sequential / read-only-parallel tool orchestration, provider retry, and cancellation.
- Minimal Echo agent end-to-end.

### Built-in tools

- File `read` / `write` / `edit` tools.
- `bash` tool with exclusive execution and a per-path `FileMutationQueue` to serialize concurrent writes.

### Adapters

- OpenAI and Anthropic providers over `reqwest` (feature-gated, rustls TLS).

### Context

- Token budgeting and truncation.
- `Compactor` context summarization.

### Sessions

- Session tree with JSONL crash recovery (append-only, fork/reduce, replay).
- Multi-lane sessions: concurrent in-process lanes writing to one session tree.
- Session concurrency hardening and explicit lane recovery semantics.

### Remote & protocols

- NDJSON remote protocol (bidirectional stream).
- Multi-session `AgentServer` (registry + lane scheduling + TCP transport).
- ACP v1 adapter (JSON-RPC 2.0 over stdio).
- Standalone CLI (interactive REPL + `--acp` mode).

### Extension

- Deferred tools: schema/execution split with lazy construction.
- Plugin registry: `Plugin` trait, async lazy instantiation, deterministic tool assembly.

### Housekeeping

- Lock-discipline cleanup (file mutation queue eviction, plugin lock, ACP test split).
- Server lane module split for the 400-line limit.
