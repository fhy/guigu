pub mod agent;
mod agent_runtime;
pub mod compactor;
pub mod context;
pub mod event;
pub mod message;
pub mod provider;
pub mod runtime;
pub mod session;
pub mod tool;
// Task 027：类型化工具参数 schema 辅助（feature `schema`，default 开启）。
#[cfg(feature = "schema")]
pub mod schema;

pub use agent::{Agent, AgentConfig, AgentError, AgentHandle, AgentSnapshot};
pub use compactor::{
    CompactionError, CompactionRequest, CompactionResult, Compactor, LlmCompactor,
    format_messages_for_summary,
};
pub use context::{CompactionPolicy, ContextBudget, default_convert_to_llm, prepare_context};
pub use provider::{
    AssistantEvent, AssistantStream, Context, Model, ModelProvider, ProviderError, ProviderRequest,
    ToolSpec,
};
pub use runtime::{AgentRuntime, LoopConfig, ToolExecutionMode};
pub use session::{
    JsonlSessionStorage, LaneHeadRecord, LaneHeadStore, LaneId, LaneWriter, NodeId, SessionEntry,
    SessionError, SessionNode, SessionRecord, SessionRecorder, SessionStorage, SessionTree,
    SharedSessionStorage, reduce,
};
pub use tool::{ResourceScope, Tool, ToolError, ToolResult};
