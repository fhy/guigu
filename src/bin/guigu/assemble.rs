//! agent 装配（Task 015，017-b 工作目录显式化）：provider / 工具 /
//! `AgentServer` / session 存储。
//!
//! 复用 007 adapters（OpenAI/Anthropic）+ 005/006 内置工具 + 013 `AgentServer` +
//! 009 `JsonlSessionStorage`。工作目录经工具构造参数显式传递（文件工具
//! `work_dir` / bash `default_cwd`），不再修改进程级 cwd（session 间隔离，017-b）。
//!
//! storage 工厂（ACP 模式经 `session/new` 建 session 用）是同步 `Fn`，而
//! `JsonlSessionStorage::open` 是 async：用 `block_in_place` + `block_on` 在
//! 多线程 runtime 上桥接；open 失败返回 `FailingStorage`（不 panic，错误进 stderr）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use guigu::core::agent::AgentConfig;
use guigu::core::message::{Message, ThinkingLevel};
use guigu::core::provider::{Model, ModelProvider};
use guigu::core::runtime::{AgentRuntime, LoopConfig};
use guigu::core::session::{
    JsonlSessionStorage, NodeId, SessionError, SessionStorage, SessionTree,
};
use guigu::core::tool::Tool;
use guigu::server::{AgentServer, SessionStorageBundle};
use guigu::tools::{BashTool, EditTool, FileMutationQueue, ReadTool, WriteTool};

use super::cli::Cli;
use super::error::CliError;
use super::provider::select_provider;

/// 缺省 system prompt：鬼谷子（Guiguzi）AI 编程助手身份。
pub const DEFAULT_SYSTEM_PROMPT: &str = "你是鬼谷子（Guiguzi），鬼谷子 AI 编程助手。\
你精通 Rust、分布式系统与高并发架构，以简洁、严谨、直接的方式协助用户分析问题、\
设计架构、编写与审查代码。";
/// 默认 lane id（REPL 单 lane）。
pub const DEFAULT_LANE: &str = "default";
/// 默认上下文窗口（token）。
const DEFAULT_CONTEXT_WINDOW: u32 = 8192;

/// 解析最终 system prompt：优先用自定义文案，缺省回退到 [`DEFAULT_SYSTEM_PROMPT`]。
///
/// 纯函数，无失败路径；在 CLI 入口一处调用一次，得到具体 `String` 后传给
/// `assemble` / `build_server`（不重复做回退）。
pub fn resolve_system_prompt(custom: Option<String>) -> String {
    custom.unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string())
}

/// 装配产物：server + 存储目录（REPL 建 session 用）。
pub struct Assembled {
    /// 013 多 session 后端。
    pub server: AgentServer,
    /// session JSONL 存储目录。
    pub log_dir: PathBuf,
}

/// 装配 server（cwd / provider / 工具 / 工厂）。REPL 与 ACP 共用。
///
/// `system_prompt` 为**已解析**的最终文案（入口经 [`resolve_system_prompt`] 回退
/// 鬼谷子默认身份后传入），本函数不重复做回退。
pub fn assemble(cli: &Cli, system_prompt: String) -> Result<Assembled, CliError> {
    // 1. 选 provider + model id（Task 022：配置优先、内联回退）。
    let selection = select_provider(cli)?;

    // 2. 工具集：005 read/write/edit + 006 bash（注入共享 FileMutationQueue）。
    //    工作目录经构造参数显式传递（017-b）：`--cwd` → 文件工具 `work_dir` +
    //    bash `default_cwd`；未指定 → `None`（相对路径按进程 cwd 解析，旧行为）。
    let tools = build_tools(cli.cwd.clone());

    // 3. server + 工厂。
    let log_dir = resolve_log_dir(&cli.log)?;
    let server = build_server(
        selection.provider,
        selection.model,
        tools,
        log_dir.clone(),
        system_prompt,
    );

    Ok(Assembled { server, log_dir })
}

/// REPL 建 session：`--session` 存在则 `load_session` 续聊，否则新建；spawn 默认 lane。
///
/// 续聊（`--session`）：`resume_lane_from_factory` 恢复 transcript（agent 可见历史
/// 上下文）+ 活动叶 head（新消息接在历史末尾，非新根）。head 传 `None`（017-b：
/// 默认回退 max NodeId 叶，CLI 续聊无分支意图）。新建：空 transcript +
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
        // head = None：回退 max NodeId 叶（CLI 续聊无分支意图，017-b）。
        assembled
            .server
            .resume_lane_from_factory(&session_id, DEFAULT_LANE, None)
            .await?;
    } else {
        assembled
            .server
            .spawn_lane_from_factory(&session_id, DEFAULT_LANE)
            .await?;
    }
    Ok(session_id)
}

/// 工具集：read/write/edit + bash（共享 `FileMutationQueue` 串行化同文件写）。
///
/// `work_dir`（017-b）：文件工具相对路径锚点 + bash 默认 cwd（`--cwd`）；
/// `None` = 相对路径按进程 cwd 解析（旧行为）。
fn build_tools(work_dir: Option<PathBuf>) -> Vec<Arc<dyn Tool>> {
    let queue = Arc::new(FileMutationQueue::new());
    vec![
        Arc::new(ReadTool::new(work_dir.clone())),
        Arc::new(WriteTool::new(queue.clone(), work_dir.clone())),
        Arc::new(EditTool::new(queue, work_dir.clone())),
        Arc::new(BashTool::new(work_dir)),
    ]
}

/// 建 server 并配置 runtime / storage 工厂。
///
/// `system_prompt` 为已解析的最终文案（见 [`assemble`]），注入 `AgentConfig`。
fn build_server(
    provider: Arc<dyn ModelProvider>,
    model: String,
    tools: Vec<Arc<dyn Tool>>,
    log_dir: PathBuf,
    system_prompt: String,
) -> AgentServer {
    let server = AgentServer::new();
    server.with_runtime_factory(move || {
        (
            AgentConfig {
                system_prompt: system_prompt.clone(),
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
    // 017-a 兼容工厂：返回 `Arc<dyn SessionStorage>`（无 head 持久化）。
    let log_dir_1 = log_dir.clone();
    server
        .with_storage_factory(move |session_id| open_storage_sync(&log_dir_1, session_id).storage);
    // 024 bundle 工厂：返回 `SessionStorageBundle`（含 head 持久化，恢复入口用）。
    server.with_storage_bundle_factory(move |session_id| open_storage_sync(&log_dir, session_id));
    server
}

/// 打开 session 存储（async，REPL 用）。
async fn open_storage(log_dir: &Path, session_id: &str) -> Result<JsonlSessionStorage, CliError> {
    let path = log_dir.join(format!("{session_id}.jsonl"));
    Ok(JsonlSessionStorage::open(path, session_id).await?)
}

/// 打开 session 存储（sync，storage 工厂用）：`block_in_place` + `block_on` 桥接 async。
///
/// 返回 `SessionStorageBundle`（`StorageFactory` 契约）：`JsonlSessionStorage` 同时
/// 实现 `SessionStorage` 与 `LaneHeadStore`，同一实例既作消息存储又作 lane head
/// 持久化（024）。open 失败返回 `FailingStorage`（`head_store = None`，不 panic）。
fn open_storage_sync(log_dir: &Path, session_id: &str) -> SessionStorageBundle {
    let path = log_dir.join(format!("{session_id}.jsonl"));
    let result = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(JsonlSessionStorage::open(path.clone(), session_id))
    });
    match result {
        Ok(storage) => {
            let storage = Arc::new(storage);
            SessionStorageBundle {
                storage: storage.clone(),
                head_store: Some(storage),
            }
        }
        Err(e) => {
            tracing::error!("failed to open session storage for {session_id}: {e}");
            SessionStorageBundle {
                storage: Arc::new(FailingStorage {
                    reason: e.to_string(),
                }),
                head_store: None,
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Command, Provider};
    use std::sync::Arc;

    #[test]
    fn resolve_system_prompt_none_falls_back_to_default() {
        assert_eq!(resolve_system_prompt(None), DEFAULT_SYSTEM_PROMPT);
    }

    #[test]
    fn resolve_system_prompt_some_uses_custom() {
        assert_eq!(resolve_system_prompt(Some("自定义".to_string())), "自定义");
    }

    /// 离线装配验证：自定义 system_prompt 经 assemble → AgentConfig → snapshot 生效。
    #[tokio::test]
    async fn assemble_injects_custom_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Some(Command::Acp),
            model: None,
            provider: Provider::Fake,
            session: None,
            cwd: None,
            log: Some(dir.path().to_path_buf()),
            api_key: None,
            base_url: None,
            system_prompt: Some("自定义身份".to_string()),
            config: None,
            api_key_env: None,
        };
        let prompt = resolve_system_prompt(cli.system_prompt.clone());
        let assembled = assemble(&cli, prompt).unwrap();
        let storage = JsonlSessionStorage::open(dir.path().join("t.jsonl"), "t")
            .await
            .unwrap();
        assembled
            .server
            .create_session("t".to_string(), Arc::new(storage))
            .await
            .unwrap();
        assembled
            .server
            .spawn_lane_from_factory("t", DEFAULT_LANE)
            .await
            .unwrap();
        let snap = assembled.server.snapshot("t", DEFAULT_LANE).await.unwrap();
        assert_eq!(snap.system_prompt, "自定义身份");
        assembled.server.shutdown().await.unwrap();
    }

    /// 离线装配验证：缺省（不传 --system-prompt）回退到鬼谷子默认身份。
    #[tokio::test]
    async fn assemble_injects_default_system_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            command: Some(Command::Acp),
            model: None,
            provider: Provider::Fake,
            session: None,
            cwd: None,
            log: Some(dir.path().to_path_buf()),
            api_key: None,
            base_url: None,
            system_prompt: None,
            config: None,
            api_key_env: None,
        };
        let prompt = resolve_system_prompt(cli.system_prompt.clone());
        let assembled = assemble(&cli, prompt).unwrap();
        let storage = JsonlSessionStorage::open(dir.path().join("t.jsonl"), "t")
            .await
            .unwrap();
        assembled
            .server
            .create_session("t".to_string(), Arc::new(storage))
            .await
            .unwrap();
        assembled
            .server
            .spawn_lane_from_factory("t", DEFAULT_LANE)
            .await
            .unwrap();
        let snap = assembled.server.snapshot("t", DEFAULT_LANE).await.unwrap();
        assert_eq!(snap.system_prompt, DEFAULT_SYSTEM_PROMPT);
        assembled.server.shutdown().await.unwrap();
    }
}
