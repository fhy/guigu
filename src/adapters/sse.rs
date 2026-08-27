//! 通用 SSE 解析器（纯逻辑：字节流 → 事件枚举）。
//!
//! 解析规则：
//! - 以空行分隔事件
//! - `data:` 多行用 `\n` 拼接
//! - `event:` 行指定事件名
//! - `data: [DONE]` 产出 [`SseEvent::Done`]
//! - 忽略 `:` 开头的注释行与未知字段行（`id`/`retry` 等）
//! - 兼容 `\r\n`

/// 单个解析出的 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// 有 `event:` 行（事件名 + data）。
    Named { event: String, data: String },
    /// 仅 `data:` 行（无事件名）。
    Data { data: String },
    /// `data: [DONE]` 终止。
    Done,
}

/// 增量 SSE 解析器：喂入字节块，返回已完成的事件。
///
/// 跨块保留未完成行与未 flush 的事件字段，故可逐块喂入网络字节。
#[derive(Debug, Default)]
pub struct SseParser {
    /// 未完成行缓冲（尚未遇到换行的字节）。
    buffer: Vec<u8>,
    /// 当前事件的 `data:` 行。
    data: Vec<String>,
    /// 当前事件的 `event:` 名。
    event: Option<String>,
}

impl SseParser {
    /// 新建空解析器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一块字节，返回其中所有已完成的事件。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        self.buffer.extend_from_slice(chunk);
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            // 剥离行尾 `\n`（必有）与 `\r`（CRLF 兼容）。
            let line = line.strip_suffix(b"\n").unwrap_or(&line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(event) = self.process_line(line) {
                events.push(event);
            }
        }
        events
    }

    /// 流结束时处理残留：先处理缓冲中无换行的最后一行，再 flush 未结束事件。
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            let line = line.strip_suffix(b"\r").unwrap_or(&line);
            if let Some(event) = self.process_line(line) {
                events.push(event);
            }
        }
        if let Some(event) = self.flush() {
            events.push(event);
        }
        events
    }

    /// 处理一行；空行触发 flush 并可能产出一个事件。
    fn process_line(&mut self, line: &[u8]) -> Option<SseEvent> {
        let text = String::from_utf8_lossy(line);
        let text: &str = text.as_ref();
        if text.is_empty() {
            return self.flush();
        }
        if text.starts_with(':') {
            return None; // 注释行。
        }
        // 解析 `field: value` 或 `field:value`（剥离至多一个前导空格）。
        let (field, value) = match text.find(':') {
            Some(idx) => {
                let rest = &text[idx + 1..];
                let value = rest.strip_prefix(' ').unwrap_or(rest);
                (&text[..idx], value)
            }
            None => (text, ""),
        };
        match field {
            "data" => self.data.push(value.to_string()),
            "event" => self.event = Some(value.to_string()),
            _ => {} // 忽略未知字段。
        }
        None
    }

    /// flush 当前事件（空行调用）；无 data 且无 event 时不产出。
    fn flush(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() && self.event.is_none() {
            return None;
        }
        let data = self.data.join("\n");
        let event = self.event.take();
        self.data.clear();
        if data == "[DONE]" {
            return Some(SseEvent::Done);
        }
        match event {
            Some(name) => Some(SseEvent::Named { event: name, data }),
            None => Some(SseEvent::Data { data }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(input: &str) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        p.feed(input.as_bytes())
    }

    #[test]
    fn single_data_line() {
        let events = feed_all("data: hello\n\n");
        assert_eq!(
            events,
            vec![SseEvent::Data {
                data: "hello".into()
            }]
        );
    }

    #[test]
    fn multi_line_data_joined_with_newline() {
        let events = feed_all("data: line1\ndata: line2\ndata: line3\n\n");
        assert_eq!(
            events,
            vec![SseEvent::Data {
                data: "line1\nline2\nline3".into()
            }]
        );
    }

    #[test]
    fn event_name_produces_named() {
        let events = feed_all("event: message_start\ndata: {\"a\":1}\n\n");
        assert_eq!(
            events,
            vec![SseEvent::Named {
                event: "message_start".into(),
                data: "{\"a\":1}".into()
            }]
        );
    }

    #[test]
    fn done_marker() {
        let events = feed_all("data: [DONE]\n\n");
        assert_eq!(events, vec![SseEvent::Done]);
    }

    #[test]
    fn blank_line_separates_events() {
        let events = feed_all("data: a\n\ndata: b\n\n");
        assert_eq!(
            events,
            vec![
                SseEvent::Data { data: "a".into() },
                SseEvent::Data { data: "b".into() },
            ]
        );
    }

    #[test]
    fn crlf_compatible() {
        let events = feed_all("data: hello\r\nevent: x\r\n\r\n");
        assert_eq!(
            events,
            vec![SseEvent::Named {
                event: "x".into(),
                data: "hello".into()
            }]
        );
    }

    #[test]
    fn comment_and_unknown_fields_ignored() {
        let events = feed_all(": comment\nid: 42\nretry: 3000\ndata: ok\n\n");
        assert_eq!(events, vec![SseEvent::Data { data: "ok".into() }]);
    }

    #[test]
    fn no_space_after_colon() {
        let events = feed_all("data:hello\n\n");
        assert_eq!(
            events,
            vec![SseEvent::Data {
                data: "hello".into()
            }]
        );
    }

    #[test]
    fn partial_line_buffered_across_feeds() {
        let mut p = SseParser::new();
        // 第一块：行未完成，无事件。
        assert!(p.feed("data: hel".as_bytes()).is_empty());
        // 第二块：补全 "data: hello" 行 + 空行 → 产出第一个事件；"da" 被缓冲。
        assert_eq!(
            p.feed("lo\n\nda".as_bytes()),
            vec![SseEvent::Data {
                data: "hello".into()
            }]
        );
        // 第三块：补全 "data: world" 行 + 空行 → 产出第二个事件。
        assert_eq!(
            p.feed("ta: world\n\n".as_bytes()),
            vec![SseEvent::Data {
                data: "world".into()
            }]
        );
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut p = SseParser::new();
        p.feed("data: tail".as_bytes());
        let events = p.finish();
        assert_eq!(
            events,
            vec![SseEvent::Data {
                data: "tail".into()
            }]
        );
    }

    #[test]
    fn finish_flushes_unterminated_done() {
        let mut p = SseParser::new();
        p.feed("data: [DONE]".as_bytes());
        let events = p.finish();
        assert_eq!(events, vec![SseEvent::Done]);
    }

    #[test]
    fn empty_event_with_only_event_name() {
        let events = feed_all("event: ping\n\n");
        assert_eq!(
            events,
            vec![SseEvent::Named {
                event: "ping".into(),
                data: String::new()
            }]
        );
    }
}
