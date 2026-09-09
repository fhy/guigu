//! CLI 面定义（Task 015）：clap derive `Parser` / `Subcommand`。
//!
//! 两个模式：**交互式 REPL**（`run`，默认可省略）与 **`acp`**（serve ACP over
//! stdio，供编辑器子进程拉起）。选项 `global = true`，子命令前后均可出现。

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// guigu：轻量级 Rust 原生 AI Agent 运行时 CLI。
#[derive(Debug, Parser)]
#[command(
    name = "guigu",
    version,
    about = "guigu: lightweight Rust-native AI agent runtime"
)]
pub struct Cli {
    /// 子命令（缺省 = `run` 交互式 REPL）。
    #[command(subcommand)]
    pub command: Option<Command>,

    /// 模型 id（如 gpt-4o-mini / claude-3-5-...）。
    #[arg(short, long, global = true)]
    pub model: Option<String>,

    /// provider：openai | anthropic（默认 openai）。
    #[arg(short, long, global = true, default_value = "openai")]
    pub provider: Provider,

    /// 加载/续用指定 session（缺省则新建）。
    #[arg(short, long, global = true)]
    pub session: Option<String>,

    /// 工作目录（默认当前目录）。
    #[arg(short, long, global = true)]
    pub cwd: Option<PathBuf>,

    /// session JSONL 存储目录（默认 ~/.local/state/guigu/ 或 env 指定）。
    #[arg(short, long, global = true)]
    pub log: Option<PathBuf>,

    /// provider API key（缺省读 env：OPENAI_API_KEY / ANTHROPIC_API_KEY）。
    #[arg(short, long, global = true)]
    pub api_key: Option<String>,

    /// 自定义 system prompt（缺省使用鬼谷子默认身份）。
    #[arg(long, global = true, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// 覆盖 provider 的 base URL（如 ModelScope/本地网关）。
    #[arg(long, global = true, value_name = "URL")]
    pub base_url: Option<String>,

    /// 配置文件路径（缺省走 `Config::resolve` 查找链：./guigu.toml →
    /// $XDG_CONFIG_HOME/guigu/config.toml → 空配置）。
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// 内联指定 API key 来源环境变量名（配合内联 `-m` 场景，可选；优先级
    /// 低于 `-k`，高于协议默认 env）。
    #[arg(long, global = true, value_name = "VAR")]
    pub api_key_env: Option<String>,
}

/// 子命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// 交互式 REPL（默认，可省略）。
    Run,
    /// 以 ACP agent 身份 serve stdio（供编辑器子进程拉起）。
    Acp,
    /// 全屏 TUI（ratatui 终端 UI，feature `tui`）。
    #[cfg(feature = "tui")]
    Tui,
}

/// LLM provider 选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Provider {
    /// OpenAI 兼容 provider。
    Openai,
    /// Anthropic provider。
    Anthropic,
    /// 离线 fake provider（测试冒烟用，`--help` 不显示）。
    #[value(hide = true)]
    Fake,
}

impl Provider {
    /// provider 名称（用于错误提示）。
    pub fn name(self) -> &'static str {
        match self {
            Provider::Openai => "openai",
            Provider::Anthropic => "anthropic",
            Provider::Fake => "fake",
        }
    }

    /// 对应的 API key 环境变量名（fake 无 key，返回空串，调用方对 fake 早退不读）。
    pub fn api_key_env(self) -> &'static str {
        match self {
            Provider::Openai => "OPENAI_API_KEY",
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Fake => "",
        }
    }

    /// 缺省模型 id（`--model` 未指定时）。
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Openai => "gpt-4o-mini",
            Provider::Anthropic => "claude-3-5-sonnet-20241022",
            Provider::Fake => "fake-model",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_system_prompt_before_subcommand() {
        let cli = Cli::try_parse_from(["guigu", "--system-prompt", "你是鬼谷子", "acp"]).unwrap();
        assert_eq!(cli.system_prompt.as_deref(), Some("你是鬼谷子"));
    }

    #[test]
    fn parse_system_prompt_after_subcommand() {
        // global = true：subcommand 后同样生效。
        let cli = Cli::try_parse_from(["guigu", "acp", "--system-prompt", "你是鬼谷子"]).unwrap();
        assert_eq!(cli.system_prompt.as_deref(), Some("你是鬼谷子"));
    }

    #[test]
    fn parse_system_prompt_absent_is_none() {
        let cli = Cli::try_parse_from(["guigu", "acp"]).unwrap();
        assert_eq!(cli.system_prompt, None);
    }

    #[test]
    fn parse_base_url_before_subcommand() {
        let cli = Cli::try_parse_from(["guigu", "--base-url", "http://localhost:11434/v1", "acp"])
            .unwrap();
        assert_eq!(cli.base_url.as_deref(), Some("http://localhost:11434/v1"));
    }

    #[test]
    fn parse_base_url_after_subcommand() {
        // global = true：subcommand 后同样生效。
        let cli = Cli::try_parse_from(["guigu", "acp", "--base-url", "http://localhost:11434/v1"])
            .unwrap();
        assert_eq!(cli.base_url.as_deref(), Some("http://localhost:11434/v1"));
    }

    #[test]
    fn parse_base_url_absent_is_none() {
        let cli = Cli::try_parse_from(["guigu", "acp"]).unwrap();
        assert_eq!(cli.base_url, None);
    }

    #[test]
    fn parse_config_path() {
        let cli = Cli::try_parse_from(["guigu", "--config", "/etc/guigu.toml", "acp"]).unwrap();
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/etc/guigu.toml"))
        );
    }

    #[test]
    fn parse_config_absent_is_none() {
        let cli = Cli::try_parse_from(["guigu", "acp"]).unwrap();
        assert_eq!(cli.config, None);
    }

    #[test]
    fn parse_api_key_env() {
        let cli = Cli::try_parse_from(["guigu", "--api-key-env", "MY_KEY", "acp"]).unwrap();
        assert_eq!(cli.api_key_env.as_deref(), Some("MY_KEY"));
    }

    #[test]
    fn parse_api_key_env_absent_is_none() {
        let cli = Cli::try_parse_from(["guigu", "acp"]).unwrap();
        assert_eq!(cli.api_key_env, None);
    }
}
