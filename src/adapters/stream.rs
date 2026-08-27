//! 共享的 SSE → `AssistantStream` 流逻辑（含取消）。
//!
//! OpenAI / Anthropic 适配器共用：逐块读 body → SSE 解析 → `map_event` 映射 →
//! 累积 → 产出 `AssistantEvent`。取消经 `tokio::select!` 打断，产出
//! `Error { aborted: true }` 后终止。

use std::collections::VecDeque;
use std::pin::Pin;

use futures::StreamExt;
use futures::stream::Stream;
use tokio_util::sync::CancellationToken;

use crate::core::provider::{AssistantEvent, AssistantStream, ProviderError};

use super::acc::Acc;
use super::sse::{SseEvent, SseParser};

/// 事件映射函数：把一个 [`SseEvent`] 转为 `AssistantEvent` 列表并更新 [`Acc`]。
pub(crate) type MapEventFn = fn(SseEvent, &mut Acc) -> Result<Vec<AssistantEvent>, ProviderError>;

/// 流状态。
struct StreamState {
    /// body 字节流（`Vec<u8>` 块）。
    body: Pin<Box<dyn Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send>>,
    parser: SseParser,
    acc: Acc,
    signal: CancellationToken,
    /// 已映射但尚未产出的事件（一块可能产出多个事件）。
    pending: VecDeque<AssistantEvent>,
    /// 是否已终止（Done / Error / 取消）。
    terminated: bool,
}

/// 从 body 字节流构建 `AssistantStream`。
///
/// `map_event` 由具体 provider 提供（OpenAI / Anthropic 映射规则不同）。
pub(crate) fn build_stream(
    body: impl Stream<Item = Result<Vec<u8>, reqwest::Error>> + Send + 'static,
    signal: CancellationToken,
    model: String,
    map_event: MapEventFn,
) -> AssistantStream {
    let state = StreamState {
        body: Box::pin(body),
        parser: SseParser::new(),
        acc: Acc::new(model),
        signal,
        pending: VecDeque::new(),
        terminated: false,
    };
    Box::pin(futures::stream::unfold(state, move |state| async move {
        step(state, map_event).await
    }))
}

/// 处理一批 [`SseEvent`]：映射并推入 pending，返回是否产出过 `Done`。
///
/// 映射失败时返回错误消息（调用方据此构造 `AssistantEvent::Error`）。
fn process_sse_events(
    state: &mut StreamState,
    sse_events: Vec<SseEvent>,
    map_event: MapEventFn,
) -> Result<bool, String> {
    let mut got_done = false;
    for sse in sse_events {
        match map_event(sse, &mut state.acc) {
            Ok(events) => {
                for e in events {
                    if matches!(e, AssistantEvent::Done { .. }) {
                        got_done = true;
                    }
                    state.pending.push_back(e);
                }
            }
            Err(err) => return Err(err.to_string()),
        }
    }
    Ok(got_done)
}

/// 推进流：循环读块/映射，直到能产出一个事件或流结束。
///
/// 返回 `Some((event, state))` 产出下一个事件；`None` 表示流结束。
/// 一块可能不产出事件（被缓冲），故需循环继续读下一块，而非直接结束。
async fn step(
    mut state: StreamState,
    map_event: MapEventFn,
) -> Option<(AssistantEvent, StreamState)> {
    loop {
        // 已终止且无残留事件 → 结束。
        if state.terminated && state.pending.is_empty() {
            return None;
        }
        // 优先产出已映射的 pending 事件（无 I/O）。
        if let Some(event) = state.pending.pop_front() {
            return Some((event, state));
        }
        // 取消 与 读块 二选一。
        tokio::select! {
            _ = state.signal.cancelled() => {
                state.terminated = true;
                return Some((
                    AssistantEvent::Error {
                        message: "cancelled".into(),
                        aborted: true,
                    },
                    state,
                ));
            }
            chunk = state.body.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        let sse_events = state.parser.feed(&bytes);
                        match process_sse_events(&mut state, sse_events, map_event) {
                            Ok(got_done) => {
                                if got_done {
                                    state.terminated = true;
                                }
                                // pending 可能为空（块被缓冲）→ 继续循环读下一块。
                            }
                            Err(message) => {
                                state.terminated = true;
                                return Some((
                                    AssistantEvent::Error {
                                        message,
                                        aborted: false,
                                    },
                                    state,
                                ));
                            }
                        }
                    }
                    Some(Err(e)) => {
                        state.terminated = true;
                        return Some((
                            AssistantEvent::Error {
                                message: e.to_string(),
                                aborted: false,
                            },
                            state,
                        ));
                    }
                    None => {
                        // body 结束：flush 残留 SSE 事件。
                        let sse_events = state.parser.finish();
                        match process_sse_events(&mut state, sse_events, map_event) {
                            Ok(got_done) => {
                                state.terminated = true;
                                if got_done {
                                    // Done 已在 pending，下一轮循环 pop 产出。
                                } else {
                                    return Some((
                                        AssistantEvent::Error {
                                            message: "stream ended without terminal event".into(),
                                            aborted: false,
                                        },
                                        state,
                                    ));
                                }
                            }
                            Err(message) => {
                                state.terminated = true;
                                return Some((
                                    AssistantEvent::Error {
                                        message,
                                        aborted: false,
                                    },
                                    state,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 测试用映射：Data/Named → TextDelta；Done → Done(build_message)。
    fn test_map_event(sse: SseEvent, acc: &mut Acc) -> Result<Vec<AssistantEvent>, ProviderError> {
        match sse {
            SseEvent::Data { data } | SseEvent::Named { data, .. } => {
                acc.append_text(&data);
                Ok(vec![AssistantEvent::TextDelta { text: data }])
            }
            SseEvent::Done => Ok(vec![AssistantEvent::Done {
                message: acc.build_message(),
            }]),
        }
    }

    fn ok_chunk(s: &str) -> Result<Vec<u8>, reqwest::Error> {
        Ok(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn yields_events_in_order_then_done() {
        let body = futures::stream::iter(vec![
            ok_chunk("data: hello\n\n"),
            ok_chunk("data: world\n\n"),
            ok_chunk("data: [DONE]\n\n"),
        ]);
        let stream = build_stream(body, CancellationToken::new(), "m".into(), test_map_event);
        let events: Vec<AssistantEvent> = stream.collect().await;
        let expected_text = AssistantEvent::TextDelta {
            text: "hello".into(),
        };
        assert_eq!(events[0], expected_text);
        assert_eq!(
            events[1],
            AssistantEvent::TextDelta {
                text: "world".into()
            }
        );
        assert!(matches!(events[2], AssistantEvent::Done { .. }));
        if let AssistantEvent::Done { message } = &events[2] {
            assert_eq!(message.content.len(), 1);
        }
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn cancellation_emits_aborted_error() {
        let body = futures::stream::pending::<Result<Vec<u8>, reqwest::Error>>();
        let signal = CancellationToken::new();
        let stream = build_stream(body, signal.clone(), "m".into(), test_map_event);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            signal.cancel();
        });
        let events: Vec<AssistantEvent> = stream.collect().await;
        assert_eq!(
            events,
            vec![AssistantEvent::Error {
                message: "cancelled".into(),
                aborted: true
            }]
        );
    }

    #[tokio::test]
    async fn ends_without_done_emits_error() {
        let body = futures::stream::iter(vec![ok_chunk("data: hello\n\n")]);
        let stream = build_stream(body, CancellationToken::new(), "m".into(), test_map_event);
        let events: Vec<AssistantEvent> = stream.collect().await;
        assert_eq!(
            events,
            vec![
                AssistantEvent::TextDelta {
                    text: "hello".into()
                },
                AssistantEvent::Error {
                    message: "stream ended without terminal event".into(),
                    aborted: false
                },
            ]
        );
    }

    #[tokio::test]
    async fn unterminated_done_at_eof_still_done() {
        let body = futures::stream::iter(vec![ok_chunk("data: [DONE]")]);
        let stream = build_stream(body, CancellationToken::new(), "m".into(), test_map_event);
        let events: Vec<AssistantEvent> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AssistantEvent::Done { .. }));
    }

    #[tokio::test]
    async fn multiple_events_in_one_chunk_all_yielded() {
        // 一块内含两个事件（两个空行分隔）。
        let body = futures::stream::iter(vec![ok_chunk("data: a\n\ndata: b\n\n")]);
        let stream = build_stream(body, CancellationToken::new(), "m".into(), test_map_event);
        let events: Vec<AssistantEvent> = stream.collect().await;
        // a, b 产出后 body 结束（无 [DONE]）→ Error。
        assert_eq!(
            events,
            vec![
                AssistantEvent::TextDelta { text: "a".into() },
                AssistantEvent::TextDelta { text: "b".into() },
                AssistantEvent::Error {
                    message: "stream ended without terminal event".into(),
                    aborted: false
                },
            ]
        );
    }
}
