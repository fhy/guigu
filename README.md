# guigu

A lightweight, Rust-native AI Agent runtime.

guigu is a trait-based, async-first runtime for building AI agents in Rust. Inspired by the architectural ideas of [pi](https://github.com/earendil-works/pi) — not a 1:1 port — and rebuilt in Rust for performance, safety, and minimal dependencies. It works both as an embeddable library and as a standalone CLI.

## Features

- **Trait-based abstractions** — `Agent`, `Tool`, `ModelProvider`, and `Plugin` are all traits. Compose the built-ins or implement your own.
- **Async-first** — built on `tokio`; non-blocking, cancellation-aware execution end to end.
- **Single-writer runtime** — one runtime task owns state. `AgentHandle` exposes commands, authoritative snapshots (`watch`), and incremental events (`broadcast`).
- **Built-in tools** — file `read`/`write`/`edit` and `bash`, with a per-path mutation queue to serialize concurrent writes.
- **Real LLM adapters** — OpenAI and Anthropic over `reqwest` (feature-gated, rustls TLS).
- **Context management** — token budgeting, truncation, and a `Compactor` for summarization.
- **Session tree + crash recovery** — append-only JSONL storage with fork/reduce and replay recovery.
- **Multi-lane sessions** — concurrent in-process lanes writing to a single session tree.
- **Remote & protocols** — an NDJSON remote protocol, a multi-session `AgentServer`, and the Agent Client Protocol (ACP v1, stdio).
- **CLI** — an interactive REPL and an `--acp` mode for editor integration.
- **Plugins & deferred tools** — lazily instantiate tools and register plugins with async initialization.

## Installation

Add `guigu` to your `Cargo.toml`:

```toml
[dependencies]
guigu = "0.1.0"
```

### Feature flags

| Feature           | Default | Description                                                        |
|-------------------|---------|--------------------------------------------------------------------|
| `providers-http`  | ✅       | OpenAI/Anthropic adapters over `reqwest` (rustls TLS)              |
| `acp-sse`         | —       | ACP SSE/HTTP transport for remote clients (reserved stub)          |

Disable default features to get a pure core library with no `reqwest` dependency:

```toml
[dependencies]
guigu = { version = "0.1.0", default-features = false }
```

## Quick start

### Library

The runtime follows a single-writer model: you drive an agent through a `Clone`-able `AgentHandle`, subscribe to its incremental event stream, and read authoritative state from its snapshot.

```rust
use guigu::{Agent, AgentHandle};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Spawn the single runtime task. `handle` is the public, Clone-able facade.
    let handle = AgentHandle::spawn(/* AgentConfig + tools */);

    // 2. Subscribe to the incremental event stream (broadcast).
    let mut events = handle.subscribe();

    // 3. Drive a turn by sending user messages into the command queue.
    handle.prompt(vec![/* Message::User(...) */]).await?;

    // 4. Wait until the run settles (after AgentEnd), then read the snapshot.
    handle.wait_for_idle().await?;
    let snapshot = handle.snapshot();

    // 5. Shut down gracefully; waits for the runtime task to exit.
    handle.shutdown().await?;
    Ok(())
}
```

The `Agent` trait contract (also implemented by `AgentHandle`) is:

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn snapshot(&self) -> AgentSnapshot;
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent>;
    async fn prompt(&self, messages: Vec<Message>) -> Result<(), AgentError>;
    async fn continue_(&self) -> Result<(), AgentError>;
    async fn steer(&self, msg: Message) -> Result<(), AgentError>;
    async fn follow_up(&self, msg: Message) -> Result<(), AgentError>;
    fn abort(&self);
    async fn wait_for_idle(&self) -> Result<(), AgentError>;
}
```

`Tool` is equally simple to implement:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Option<serde_json::Value>;
    fn resource_scope(&self) -> ResourceScope; // ReadOnly | FileWriter | Exclusive
    async fn execute(
        &self,
        tool_call_id: &str,
        args: serde_json::Value,
        signal: CancellationToken,
        on_update: Option<&dyn Fn(ToolResult)>,
    ) -> Result<ToolResult, ToolError>;
}
```

The snippets above are illustrative of the lifecycle; for authoritative, runnable examples see `tests/` and the full API contract in [docs/architecture.md](docs/architecture.md) (§3).

### CLI

Build and run the standalone binary:

```text
cargo run -- guigu run
```

```
guigu [OPTIONS] [COMMAND]

Commands:
  run      Interactive REPL (default; can be omitted)
  acp      Serve ACP over stdio (for editor subprocess integration)
  help

Options:
  -m, --model <MODEL>      Model id (e.g. gpt-4o-mini / claude-3-5-...)
  -p, --provider <PROVIDER>  openai | anthropic (default: openai)
  -s, --session <ID>       Load/resume a session (creates a new one if absent)
  -c, --cwd <DIR>          Working directory (default: current directory)
  -l, --log <DIR>          Session JSONL storage dir
  -k, --api-key <KEY>      Provider API key (else reads OPENAI_API_KEY / ANTHROPIC_API_KEY)
```

- `guigu run` starts an interactive REPL. Send a prompt and watch text/tool progress stream to stdout; `/quit` or Ctrl-D exits.
- `guigu acp` serves the Agent Client Protocol (JSON-RPC 2.0) over stdio, so an editor can spawn it as a subprocess.

## Architecture

For the full design — concurrency model, message/event model, tool orchestration, and module layout — see [docs/architecture.md](docs/architecture.md). Task history and status live in [docs/TASK_BOARD.md](docs/TASK_BOARD.md).

## License

MIT. See [LICENSE](LICENSE).
