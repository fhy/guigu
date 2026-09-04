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
}

/// 子命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// 交互式 REPL（默认，可省略）。
    Run,
    /// 以 ACP agent 身份 serve stdio（供编辑器子进程拉起）。
    Acp,
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
