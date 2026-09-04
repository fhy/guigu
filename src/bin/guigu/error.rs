//! CLI 错误类型（Task 015）。
//!
//! 统一映射为退出码：装配失败 / 参数非法 → 非零退出 + stderr 提示；
//! REPL 内单次 prompt 错误打印但不退出（由 REPL 循环处理）。
//!
//! `main` 返回 `ExitCode`，内部 `run()` 返回 `Result<(), CliError>`：错误经
//! `Display` 打印到 stderr 并以非零码退出（`main() -> Result<(), E>` 默认走
//! `Debug`，故不直接用 `Result` 作 `main` 返回类型）。

use guigu::acp::AcpError;
use guigu::core::provider::ProviderError;
use guigu::core::session::SessionError;
use guigu::server::ServerError;

/// CLI 层错误。
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// provider 构造失败。
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),
    /// 缺少 API key。
    #[error("missing API key for {provider}: set --api-key or the {env} environment variable")]
    MissingApiKey {
        /// provider 名称。
        provider: String,
        /// 对应的环境变量名。
        env: &'static str,
    },
    /// session 存储错误。
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    /// server 错误。
    #[error("server error: {0}")]
    Server(#[from] ServerError),
    /// ACP 错误。
    #[error("acp error: {0}")]
    Acp(#[from] AcpError),
    /// IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
