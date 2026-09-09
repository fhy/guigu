//! TUI 事件循环测试（Task 023 r1 修复）：prompt 提交不阻塞 UI 事件循环。
//!
//! 从 `mod.rs` 拆出（单文件 ≤ 400 行约束），经 `#[path]` 挂为 `tui::loop_tests`。
//! 无真实终端驱动 `run_loop`（draw 回调计数）：
//! - prompt 未完成（命令 task 未回送）期间，循环仍能消费键事件与 agent 事件；
//! - prompt 提交失败回送 UI 状态（error + Error 状态）；
//! - 终端读错误以明确错误退出循环（区分正常退出）。

use super::*;
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyModifiers};
use guigu::core::agent::AgentConfig;
use guigu::core::message::{AssistantContent, AssistantMessage, ThinkingLevel};
use guigu::core::provider::AssistantEvent;
use guigu::core::runtime::{AgentRuntime, LoopConfig};
use guigu::core::session::{
    NodeId, SessionEntry, SessionError, SessionStorage, SessionTree, reduce,
};
use std::sync::atomic::{AtomicU64, AtomicUsize};

use crate::fake::FakeProvider;

/// 发送一个键事件到循环。
fn send_key(tx: &mpsc::UnboundedSender<ReaderEvent>, code: KeyCode) {
    let _ = tx.send(ReaderEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

/// 内存 `SessionStorage`（测试用，避免 `JsonlSessionStorage::open` 的 async 约束）。
struct MemStorage {
    entries: std::sync::Mutex<Vec<SessionEntry>>,
    next_id: AtomicU64,
}

impl MemStorage {
    fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

#[async_trait]
impl SessionStorage for MemStorage {
    async fn append(
        &self,
        parent_id: Option<NodeId>,
        message: Message,
    ) -> Result<NodeId, SessionError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.entries.lock().unwrap().push(SessionEntry {
            id,
            parent_id,
            message,
        });
        Ok(id)
    }

    async fn load(&self) -> Result<SessionTree, SessionError> {
        reduce(self.entries.lock().unwrap().clone())
    }

    fn next_id(&self) -> NodeId {
        self.next_id.load(Ordering::SeqCst)
    }
}

/// prompt 未完成（命令 task 未回送）期间，循环仍能消费键事件与 agent 事件。
#[tokio::test]
async fn prompt_in_flight_does_not_block_event_loop() {
    // agent 事件 channel（测试直接喂事件）。
    let (event_tx, mut rx) = broadcast::channel::<AgentEvent>(16);

    // 键盘 channel。
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<ReaderEvent>();

    // 命令 channel + 假命令 task：收到 Prompt 先信号「进行中」，再延迟 300ms 回送。
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();
    let (result_tx, mut result_rx) = mpsc::unbounded_channel::<CommandResult>();
    let (prompt_started_tx, prompt_started_rx) = tokio::sync::oneshot::channel::<()>();
    let mut prompt_started_tx = Some(prompt_started_tx);
    let fake_cmd = tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                TuiCommand::Prompt { .. } => {
                    if let Some(tx) = prompt_started_tx.take() {
                        let _ = tx.send(());
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let _ = result_tx.send(CommandResult::PromptAccepted);
                }
                TuiCommand::Abort => {}
            }
        }
    });

    // 无头循环（draw 回调计数，无真实终端）。
    let draws = Arc::new(AtomicUsize::new(0));
    let draws_clone = draws.clone();
    let mut state = TuiState::new("m".into(), "l".into());
    let loop_task = tokio::spawn(async move {
        let mut draw = |_s: &TuiState| {
            draws_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let result = run_loop(
            &mut draw,
            &mut state,
            &mut rx,
            &mut key_rx,
            &mut result_rx,
            &cmd_tx,
        )
        .await;
        (result, state)
    });

    // 输入 "hi" 并提交 → 循环把 Prompt 发给命令 task。
    send_key(&key_tx, KeyCode::Char('h'));
    send_key(&key_tx, KeyCode::Char('i'));
    send_key(&key_tx, KeyCode::Enter);

    // 等 prompt 进入「未完成」窗口（命令 task 已收到并 sleep 300ms）。
    tokio::time::timeout(Duration::from_millis(2000), prompt_started_rx)
        .await
        .expect("prompt should be submitted within 2s")
        .expect("prompt started signal");

    // prompt 未完成期间：循环应仍能处理键 + agent 事件。
    let draws_before = draws.load(Ordering::SeqCst);
    send_key(&key_tx, KeyCode::Char('x'));
    let _ = event_tx.send(AgentEvent::MessageUpdate {
        message: Arc::new(Message::Assistant(AssistantMessage {
            content: vec![AssistantContent::Text { text: "ok".into() }],
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
            timestamp: 0,
        })),
        assistant_event: AssistantEvent::TextDelta { text: "ok".into() },
    });

    // 等循环消费（仍在 300ms 未完成窗口内）。
    tokio::time::sleep(Duration::from_millis(150)).await;
    // prompt 未完成期间循环持续重绘（若被阻塞则无重绘）。
    assert!(
        draws.load(Ordering::SeqCst) > draws_before,
        "loop should keep drawing while prompt is in flight"
    );

    // 退出。
    send_key(&key_tx, KeyCode::Esc);
    let (result, state) = loop_task.await.expect("loop task");
    assert!(result.is_ok(), "loop should exit cleanly: {result:?}");

    // 键在 prompt 未完成期间被处理（循环若阻塞则 input 为空）。
    assert_eq!(state.input, "x");
    // agent 事件在 prompt 未完成期间被消费（流式气泡）。
    assert_eq!(
        state.streaming.as_ref().map(|s| s.text.as_str()),
        Some("ok")
    );

    // cmd_tx 随 loop task 移动并 drop → 假命令 task 退出。
    let _ = fake_cmd.await;
}

/// prompt 提交失败（lane 不存在）回送 UI 状态（error + Error 状态）。
#[tokio::test]
async fn prompt_failure_reported_to_ui() {
    // 真 server + 真命令 task：session 存在但未 spawn lane → prompt 返回 LaneNotFound。
    let server = AgentServer::new();
    server.with_runtime_factory(|| {
        (
            AgentConfig {
                system_prompt: "t".into(),
                model: Some("m".into()),
                thinking_level: ThinkingLevel::Off,
            },
            AgentRuntime {
                provider: Arc::new(FakeProvider),
                tools: Vec::new(),
                loop_config: LoopConfig::default(),
            },
        )
    });
    server
        .create_session("s".into(), Arc::new(MemStorage::new()))
        .await
        .expect("create session");

    // _event_tx 持有到循环退出（drop 会关闭 channel 使循环提前退出）。
    let (_event_tx, mut rx) = broadcast::channel::<AgentEvent>(16);
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<ReaderEvent>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();
    let (result_tx, mut result_rx) = mpsc::unbounded_channel::<CommandResult>();
    let cmd_task = spawn_command_task(server.clone(), "s", "l", cmd_rx, result_tx);

    let draws = Arc::new(AtomicUsize::new(0));
    let draws_clone = draws.clone();
    let mut state = TuiState::new("m".into(), "l".into());
    let loop_task = tokio::spawn(async move {
        let mut draw = |_s: &TuiState| {
            draws_clone.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let result = run_loop(
            &mut draw,
            &mut state,
            &mut rx,
            &mut key_rx,
            &mut result_rx,
            &cmd_tx,
        )
        .await;
        (result, state)
    });

    // 提交 prompt → 命令 task 回送 PromptFailed（lane not found）。
    send_key(&key_tx, KeyCode::Char('h'));
    send_key(&key_tx, KeyCode::Enter);

    // 等失败回送被循环消费（均为亚毫秒操作，300ms 留足余量）。
    tokio::time::sleep(Duration::from_millis(300)).await;
    send_key(&key_tx, KeyCode::Esc);
    let (result, state) = loop_task.await.expect("loop task");
    assert!(result.is_ok(), "loop should exit cleanly: {result:?}");
    assert_eq!(state.status, Status::Error);
    assert!(
        state
            .error
            .as_deref()
            .unwrap_or("")
            .contains("lane not found"),
        "expected lane-not-found error, got: {:?}",
        state.error
    );

    // cmd_tx 随 loop task 移动并 drop → 命令 task 退出。
    let _ = cmd_task.await;
    let _ = server.shutdown().await;
}

/// 终端读错误以明确错误退出循环（区分正常退出）。
#[tokio::test]
async fn reader_error_exits_loop_with_error() {
    let (_event_tx, mut rx) = broadcast::channel::<AgentEvent>(16);
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<ReaderEvent>();
    let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();
    let (_result_tx, mut result_rx) = mpsc::unbounded_channel::<CommandResult>();

    let mut state = TuiState::new("m".into(), "l".into());
    let loop_task = tokio::spawn(async move {
        let mut draw = |_s: &TuiState| Ok(());
        run_loop(
            &mut draw,
            &mut state,
            &mut rx,
            &mut key_rx,
            &mut result_rx,
            &cmd_tx,
        )
        .await
    });

    let _ = key_tx.send(ReaderEvent::Error("terminal disconnected".into()));
    let result = loop_task.await.expect("loop task");
    let err = result.expect_err("reader error should exit the loop with an error");
    assert!(
        err.to_string().contains("terminal disconnected"),
        "got: {err}"
    );
}
