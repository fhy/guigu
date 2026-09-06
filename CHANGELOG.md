# Changelog

All notable changes to guigu are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
