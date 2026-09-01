//! newline-delimited JSON 帧编解码（在线双向流）。
//!
//! 帧格式：每条消息序列化为**单行 JSON**，以 `\n` 结尾；反序列化按行
//! `read_line`。空行忽略；某行 `serde_json` 解析失败 → `RemoteError::Protocol`
//! （与 009 JSONL 的「尾部半行跳过」不同：本协议是双向在线流，任何坏帧都属
//! 协议违规，终止连接）。
//!
//! 对 `AsyncRead + AsyncWrite + Send + Unpin` 的泛型字节流工作。写侧由调用方
//! 用 `tokio::io::split` 拆分读写半，写半归单一 writer task（`mpsc` 汇入 →
//! 写循环），避免多任务持锁跨 await。

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use super::protocol::RemoteError;

/// 把一条消息编码为单行 JSON（以 `\n` 结尾）。
pub fn encode_line<T: Serialize>(msg: &T) -> Result<Vec<u8>, RemoteError> {
    let mut bytes = serde_json::to_vec(msg)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// 解码一行 JSON。空行（仅空白）返回 `Ok(None)`；坏 JSON 返回 `Protocol` 错误。
pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<Option<T>, RemoteError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let msg = serde_json::from_str(trimmed)
        .map_err(|e| RemoteError::Protocol(format!("invalid frame: {e}")))?;
    Ok(Some(msg))
}

/// 行读取器：从 `AsyncRead` 逐行读取并解码。
///
/// 内部用 `BufReader` 缓冲，天然处理「一行拆半写」（partial line）：
/// `read_line` 会缓冲到看到 `\n` 才返回，故跨多次底层 `read` 的半行也能正确
/// 重组。
pub struct LineReader<R> {
    inner: BufReader<R>,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    /// 包装一个 `AsyncRead` 为行读取器。
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
        }
    }

    /// 读取下一条消息。EOF 返回 `Ok(None)`；空行跳过；坏 JSON 返回 `Protocol`。
    pub async fn next<T: DeserializeOwned>(&mut self) -> Result<Option<T>, RemoteError> {
        loop {
            let mut line = String::new();
            let n = self.inner.read_line(&mut line).await?;
            if n == 0 {
                return Ok(None);
            }
            if let Some(msg) = decode_line::<T>(&line)? {
                return Ok(Some(msg));
            }
            // 空行：继续读下一行。
        }
    }
}

/// 写一条消息到字节流（单行 JSON + flush）。
pub async fn write_line<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &impl Serialize,
) -> Result<(), RemoteError> {
    let bytes = encode_line(msg)?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::protocol::{RemoteRequest, ServerMessage};
    use tokio::io::{AsyncWriteExt, duplex};

    // duplex 语义：写入 `client` 端的数据可从 `server` 端读出。故写侧用
    // `client`，读侧用 `LineReader::new(server)`。

    /// 单行 roundtrip：编码后解码应还原。
    #[tokio::test]
    async fn test_single_line_roundtrip() {
        let (mut client, server) = duplex(1024);
        let mut reader = LineReader::new(server);

        let msg = RemoteRequest::Continue { id: 42 };
        write_line(&mut client, &msg).await.expect("write");

        let decoded = reader
            .next::<RemoteRequest>()
            .await
            .expect("read")
            .expect("some");
        assert_eq!(decoded, msg);
    }

    /// 多行合并读：一次性写入多行，逐条读出。
    #[tokio::test]
    async fn test_multi_line_merged_read() {
        let (mut client, server) = duplex(4096);
        let mut reader = LineReader::new(server);

        let msgs = vec![
            RemoteRequest::Continue { id: 1 },
            RemoteRequest::Abort { id: 2 },
            RemoteRequest::Reset { id: 3 },
        ];
        // 合并为一次 write_all（模拟多行到达同一 read buffer）。
        let mut buf = Vec::new();
        for m in &msgs {
            buf.extend(encode_line(m).expect("encode"));
        }
        client.write_all(&buf).await.expect("write");
        client.flush().await.expect("flush");

        for expected in &msgs {
            let decoded = reader
                .next::<RemoteRequest>()
                .await
                .expect("read")
                .expect("some");
            assert_eq!(&decoded, expected);
        }
    }

    /// 一行拆半写（partial line）：分两次写半行，读侧应正确重组。
    #[tokio::test]
    async fn test_partial_line_write() {
        let (mut client, server) = duplex(1024);
        let mut reader = LineReader::new(server);

        let msg = RemoteRequest::GetSnapshot { id: 7 };
        let full = encode_line(&msg).expect("encode");
        let mid = full.len() / 2;
        // 分两次写：前半 + 后半（含 \n）。
        client.write_all(&full[..mid]).await.expect("write part1");
        client.flush().await.expect("flush part1");
        client.write_all(&full[mid..]).await.expect("write part2");
        client.flush().await.expect("flush part2");

        let decoded = reader
            .next::<RemoteRequest>()
            .await
            .expect("read")
            .expect("some");
        assert_eq!(decoded, msg);
    }

    /// 空行忽略：空行被跳过，读到下一条有效消息。
    #[tokio::test]
    async fn test_empty_line_ignored() {
        let (mut client, server) = duplex(1024);
        let mut reader = LineReader::new(server);

        // 写一个空行 + 一个有效消息。
        client.write_all(b"\n").await.expect("write empty");
        client.flush().await.expect("flush empty");
        write_line(&mut client, &RemoteRequest::Abort { id: 9 })
            .await
            .expect("write msg");

        let decoded = reader
            .next::<RemoteRequest>()
            .await
            .expect("read")
            .expect("some");
        assert_eq!(decoded, RemoteRequest::Abort { id: 9 });
    }

    /// 坏 JSON → `Protocol` 错误。
    #[tokio::test]
    async fn test_bad_json_protocol_error() {
        let (mut client, server) = duplex(1024);
        let mut reader = LineReader::new(server);

        client
            .write_all(b"this is not json\n")
            .await
            .expect("write bad");
        client.flush().await.expect("flush");

        let result = reader.next::<RemoteRequest>().await;
        match result {
            Err(RemoteError::Protocol(_)) => {}
            other => panic!("expected Protocol error, got {other:?}"),
        }
    }

    /// EOF：写端关闭后，读侧返回 `Ok(None)`。
    #[tokio::test]
    async fn test_eof_returns_none() {
        let (mut client, server) = duplex(1024);
        let mut reader = LineReader::new(server);

        write_line(
            &mut client,
            &ServerMessage::Event {
                event: crate::core::event::AgentEvent::AgentStart,
            },
        )
        .await
        .expect("write");
        drop(client); // 关闭写端，触发 EOF

        let first = reader
            .next::<ServerMessage>()
            .await
            .expect("read")
            .expect("some");
        assert!(matches!(first, ServerMessage::Event { .. }));
        let eof = reader.next::<ServerMessage>().await.expect("read");
        assert!(eof.is_none(), "expected None on EOF");
    }
}
