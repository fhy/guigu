//! agent 装配（Task 015）：provider / 工具 / `AgentServer` / session 存储。
//!
//! 复用 007 adapters（OpenAI/Anthropic）+ 005/006 内置工具 + 013 `AgentServer` +
//! 009 `JsonlSessionStorage`。写入工具经 `std::env::set_current_dir` 锚定 `--cwd`
//! （相对路径解析到工作目录）。
//!
//! storage 工厂（ACP 模式经 `session/new` 建 session 用）是同步 `Fn`，而
//! `JsonlSessionStorage::open` 是 async：用 `block_in_place` + `block_on` 在
//! 多线程 runtime 上桥接；open 失败返回 `FailingStorage`（不 panic，错误进 stderr）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use guigu::adapters::{AnthropicConfig, AnthropicProvider, OpenAiConfig, OpenAiProvider};
use guigu::core::agent::AgentConfig;
use guigu::core::message::{Message, ThinkingLevel};
use guigu::core::provider::{Model, ModelProvider};
use guigu::core::runtime::{AgentRuntime, LoopConfig};
use guigu::core::session::{
    JsonlSessionStorage, NodeId, SessionError, SessionStorage, SessionTree,
};
use guigu::core::tool::Tool;
use guigu::server::AgentServer;
use guigu::tools::{BashTool, EditTool, FileMutationQueue, ReadTool, WriteTool};

use super::cli::{Cli, Provider};
use super::error::CliError;
use super::fake::FakeProvider;

/// 默认 system prompt。
const SYSTEM_PROMPT: &str = "You are guigu, a helpful coding assistant.";
/// 默认 lane id（REPL 单 lane）。
pub const DEFAULT_LANE: &str = "default";
/// 默认上下文窗口（token）。
const DEFAULT_CONTEXT_WINDOW: u32 = 8192;

/// 装配产物：server + 存储目录（REPL 建 session 用）。
pub struct Assembled {
    /// 013 多 session 后端。
    pub server: AgentServer,
    /// session JSONL 存储目录。
    pub log_dir: PathBuf,
}

/// 装配 server（cwd / provider / 工具 / 工厂）。REPL 与 ACP 共用。
pub fn assemble(cli: &Cli) -> Result<Assembled, CliError> {
    // 1. 锚定工作目录：相对路径（read/write/edit/bash）解析到 --cwd。
    if let Some(cwd) = &cli.cwd {
        std::env::set_current_dir(cwd)?;
    }

    // 2. 选 provider（读 --api-key / env）。
    let provider = build_provider(cli)?;
    let model = cli
        .model
        .clone()
        .unwrap_or_else(|| cli.provider.default_model().to_string());

    // 3. 工具集：005 read/write/edit + 006 bash（注入共享 FileMutationQueue）。
    let tools = build_tools();

    // 4. server + 工厂。
    let log_dir = resolve_log_dir(&cli.log)?;
    let server = build_server(provider, model, tools, log_dir.clone());

    Ok(Assembled { server, log_dir })
}

/// REPL 建 session：`--session` 存在则 `load_session` 续聊，否则新建；spawn 默认 lane。
///
/// 续聊（`--session`）：`resume_lane_from_factory` 恢复 transcript（agent 可见历史
/// 上下文）+ 活动叶 head（新消息接在历史末尾，非新根）。新建：空 transcript +
/// head `None`（首次 append 成为根）。
pub async fn setup_session(assembled: &Assembled, cli: &Cli) -> Result<String, CliError> {
    let resume = cli.session.is_some();
    let session_id = match &cli.session {
        Some(id) => {
            let storage = open_storage(&assembled.log_dir, id).await?;
            // 传裸 storage；server 在 load_session 边界包成 SharedSessionStorage。
            assembled
                .server
                .load_session(id.clone(), Arc::new(storage))
                .await?;
            id.clone()
        }
        None => {
            let id = generate_session_id();
            let storage = open_storage(&assembled.log_dir, &id).await?;
            // 传裸 storage；server 在 create_session 边界包成 SharedSessionStorage。
            assembled
                .server
                .create_session(id.clone(), Arc::new(storage))
                .await?;
            id
        }
    };
    if resume {
        assembled
            .server
            .resume_lane_from_factory(&session_id, DEFAULT_LANE)
            .await?;
    } else {
        assembled
            .server
            .spawn_lane_from_factory(&session_id, DEFAULT_LANE)
            .await?;
    }
    Ok(session_id)
}

/// 选 provider：fake 早退（离线冒烟，无 key）；真 provider 读 `--api-key` 或对应
/// env，缺 key → `MissingApiKey`。
fn build_provider(cli: &Cli) -> Result<Arc<dyn ModelProvider>, CliError> {
    if matches!(cli.provider, Provider::Fake) {
        return Ok(Arc::new(FakeProvider));
    }
    let env = cli.provider.api_key_env();
    let key = cli
        .api_key
        .clone()
        .or_else(|| std::env::var(env).ok())
        .ok_or_else(|| CliError::MissingApiKey {
            provider: cli.provider.name().to_string(),
            env,
        })?;
    match cli.provider {
        Provider::Openai => Ok(Arc::new(OpenAiProvider::new(OpenAiConfig::new(key))?)),
        Provider::Anthropic => Ok(Arc::new(AnthropicProvider::new(AnthropicConfig::new(key))?)),
        // 防御分支：上方已对 Fake 早退，此处仅为穷尽 match（不 panic）。
        Provider::Fake => Ok(Arc::new(FakeProvider)),
    }
}

/// 工具集：read/write/edit + bash（共享 `FileMutationQueue` 串行化同文件写）。
fn build_tools() -> Vec<Arc<dyn Tool>> {
    let queue = Arc::new(FileMutationQueue::new());
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool::new(queue.clone())),
        Arc::new(EditTool::new(queue)),
        Arc::new(BashTool),
    ]
}

/// 建 server 并配置 runtime / storage 工厂。
fn build_server(
    provider: Arc<dyn ModelProvider>,
    model: String,
    tools: Vec<Arc<dyn Tool>>,
    log_dir: PathBuf,
) -> AgentServer {
    let server = AgentServer::new();
    server.with_runtime_factory(move || {
        (
            AgentConfig {
                system_prompt: SYSTEM_PROMPT.to_string(),
                model: Some(model.clone()),
                thinking_level: ThinkingLevel::Off,
            },
            AgentRuntime {
                provider: provider.clone(),
                tools: tools.clone(),
                loop_config: LoopConfig {
                    model: Model {
                        id: model.clone(),
                        context_window: DEFAULT_CONTEXT_WINDOW,
                    },
                    ..LoopConfig::default()
                },
            },
        )
    });
    server.with_storage_factory(move |session_id| open_storage_sync(&log_dir, session_id));
    server
}

/// 打开 session 存储（async，REPL 用）。
async fn open_storage(log_dir: &Path, session_id: &str) -> Result<JsonlSessionStorage, CliError> {
    let path = log_dir.join(format!("{session_id}.jsonl"));
    Ok(JsonlSessionStorage::open(path, session_id).await?)
}

/// 打开 session 存储（sync，storage 工厂用）：`block_in_place` + `block_on` 桥接 async。
///
/// 返回裸 `Arc<dyn SessionStorage>`（`StorageFactory` 契约）；server 在
/// `create_session` / `load_session` 边界统一包成 `Arc<SharedSessionStorage>`。
fn open_storage_sync(log_dir: &Path, session_id: &str) -> Arc<dyn SessionStorage> {
    let path = log_dir.join(format!("{session_id}.jsonl"));
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(JsonlSessionStorage::open(path.clone(), session_id))
    });
    match result {
        Ok(storage) => Arc::new(storage),
        Err(e) => {
            tracing::error!("failed to open session storage for {session_id}: {e}");
            Arc::new(FailingStorage {
                reason: e.to_string(),
            })
        }
    }
}

/// 解析 session 存储目录：`--log` > `GUIGU_STATE_DIR` > `XDG_STATE_HOME/guigu` >
/// `~/.local/state/guigu` > `./guigu-state`。
fn resolve_log_dir(log: &Option<PathBuf>) -> Result<PathBuf, CliError> {
    if let Some(dir) = log {
        return Ok(dir.clone());
    }
    if let Ok(dir) = std::env::var("GUIGU_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(xdg).join("guigu"));
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(home).join(".local/state/guigu"));
    }
    Ok(PathBuf::from("guigu-state"))
}

/// 生成新 session id（时间戳 + pid，避免 `unwrap`）。
fn generate_session_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("s-{ts}-{}", std::process::id())
}

/// open 失败时的兜底存储：所有操作返回明确错误（不 panic）。
struct FailingStorage {
    reason: String,
}

#[async_trait]
impl SessionStorage for FailingStorage {
    async fn append(
        &self,
        _parent_id: Option<NodeId>,
        _message: Message,
    ) -> Result<NodeId, SessionError> {
        Err(io_other(self.reason.clone()))
    }

    async fn load(&self) -> Result<SessionTree, SessionError> {
        Err(io_other(self.reason.clone()))
    }

    fn next_id(&self) -> NodeId {
        0
    }
}

fn io_other(reason: String) -> SessionError {
    SessionError::Io(std::io::Error::other(reason))
}
