//! `context` 模块单元测试（续）：`truncate_to_budget` 拓扑安全截断 +
//! `keep_recent` turn 粒度。

use super::tests::{FakeCompactor, assistant_tool_call, tool_result, user_msg};
use super::*;

// ---------- truncate_to_budget ----------

/// 未超预算：原样返回。
#[test]
fn test_truncate_within_budget() {
    let msgs = vec![user_msg("a"), user_msg("b")];
    let out = truncate_to_budget(&msgs, 10_000);
    assert_eq!(out, msgs, "未超预算应原样返回");
}

/// 超预算：从头部整 turn 丢弃，切点落在 User 边界。
#[test]
fn test_truncate_drops_whole_turns() {
    // 5 条用户消息，每条 400 字节 ≈ 101 token。窗口 250 → 最多放 2 条。
    let msgs: Vec<Arc<Message>> = (0..5)
        .map(|i| user_msg(&format!("m{i}{}", "x".repeat(400))))
        .collect();
    let out = truncate_to_budget(&msgs, 250);
    assert!(out.len() <= 2, "应只保留最近的消息");
    // 切点必须落在 User 边界（所有消息都是 User，故任意切点都合法）。
    assert!(
        matches!(out[0].as_ref(), Message::User(_)),
        "切点应落在 User 边界"
    );
    // 最近一条必须保留。
    assert_eq!(out.last().unwrap().as_ref(), msgs.last().unwrap().as_ref());
}

/// 拓扑安全：截断点落在 Assistant(ToolCall)/ToolResult 之间时，
/// 断言截断后请求不以 ToolResult 开头、tool call/result 成组保留。
#[test]
fn test_truncate_topology_safe_tool_call_result() {
    // 构造 transcript：
    // [User, Assistant(ToolCall), ToolResult, User, Assistant(ToolCall), ToolResult, User]
    // 每条消息 400 字节 ≈ 101 token。窗口 250 → 最多放 2 条。
    let u0 = user_msg(&format!("u0{}", "x".repeat(400)));
    let a0 = assistant_tool_call("c0", "tool");
    let t0 = tool_result("c0", "tool", &"y".repeat(400));
    let u1 = user_msg(&format!("u1{}", "x".repeat(400)));
    let a1 = assistant_tool_call("c1", "tool");
    let t1 = tool_result("c1", "tool", &"y".repeat(400));
    let u2 = user_msg(&format!("u2{}", "x".repeat(400)));
    let msgs = vec![u0, a0, t0, u1, a1, t1, u2];

    let out = truncate_to_budget(&msgs, 250);
    // 截断后不应以 ToolResult 开头（孤立 ToolResult）。
    assert!(
        !matches!(out.first().unwrap().as_ref(), Message::ToolResult(_)),
        "截断后不应以 ToolResult 开头"
    );
    // 切点必须落在 User 边界。
    assert!(
        matches!(out[0].as_ref(), Message::User(_)),
        "切点应落在 User 边界"
    );
    // 最近一条必须保留。
    assert_eq!(out.last().unwrap().as_ref(), msgs.last().unwrap().as_ref());
}

/// 单 turn 超预算：至少保留最后 1 个 turn，不产出空列表。
#[test]
fn test_truncate_keeps_last_turn_when_oversized() {
    // 单条消息就超窗口：截断后仍保留最近一条（不丢空）。
    let big = user_msg(&"x".repeat(1000));
    let msgs = vec![big.clone(), big.clone()];
    let out = truncate_to_budget(&msgs, 10);
    assert_eq!(out.len(), 1, "超窗单条也应保留最近一条");
}

/// 防御：首条非 User（不应发生），仍以首条为切点，且不切断 tool call/result 成组。
///
/// 构造畸形 transcript（首条为 `Assistant(ToolCall)`，无前置 `User`），含完整
/// tool call/result 成组。断言截断后请求不以孤立 `ToolResult` 开头、成组保留。
#[test]
fn test_truncate_defensive_first_not_user() {
    // 畸形 transcript（首条非 User，不应发生）：
    // [Assistant(ToolCall c0), ToolResult c0, User, Assistant(ToolCall c1), ToolResult c1, User]
    // User 消息 1002 字节 ≈ 251 token；tool call/result 较小（~3 / ~103）。
    let a0 = assistant_tool_call("c0", "tool");
    let t0 = tool_result("c0", "tool", &"y".repeat(400));
    let u0 = user_msg(&format!("u0{}", "x".repeat(1000)));
    let a1 = assistant_tool_call("c1", "tool");
    let t1 = tool_result("c1", "tool", &"y".repeat(400));
    let u1 = user_msg(&format!("u1{}", "x".repeat(1000)));
    let msgs = vec![a0, t0, u0, a1, t1, u1];

    // 预算 650：整体（~714）超预算，但 messages[2..]（~608）满足。
    let out = truncate_to_budget(&msgs, 650);

    // 不应产出空列表。
    assert!(!out.is_empty(), "不应产出空列表");
    // 切点落在 User 边界：不以孤立 ToolResult 开头。
    assert!(
        !matches!(out.first().unwrap().as_ref(), Message::ToolResult(_)),
        "截断后不应以孤立 ToolResult 开头"
    );
    assert!(
        matches!(out.first().unwrap().as_ref(), Message::User(_)),
        "切点应落在 User 边界"
    );
    // tool call/result 成组保留：c1 的 Assistant(ToolCall) 与 ToolResult 相邻。
    assert_eq!(
        out.len(),
        4,
        "应保留 [User, Assistant(c1), ToolResult(c1), User]"
    );
    assert!(
        matches!(out[1].as_ref(), Message::Assistant(a)
            if a.content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)))),
        "第二条应为 Assistant(ToolCall c1)"
    );
    assert!(
        matches!(out[2].as_ref(), Message::ToolResult(_)),
        "第三条应为 ToolResult c1（与 c1 成组）"
    );
    // 最近一条必须保留。
    assert_eq!(out.last().unwrap().as_ref(), msgs.last().unwrap().as_ref());
}

// ---------- keep_recent turn 粒度 ----------

/// keep_recent 按 turn 粒度：保留最近 N 个完整 turn。
#[tokio::test]
async fn test_keep_recent_turn_granularity() {
    let compactor = FakeCompactor::ok("SUMMARY");
    let policy = CompactionPolicy {
        budget_tokens: 200,
        keep_recent: 2, // 保留最近 2 个完整 turn。
        reserve_output_tokens: 0,
        protocol_wrapper_tokens: 0,
    };
    // 构造 3 个 turn：
    // Turn 1: [User, Assistant(ToolCall), ToolResult]
    // Turn 2: [User, Assistant(ToolCall), ToolResult]
    // Turn 3: [User]
    // 每条消息 400 字节 ≈ 101 token，共 7 条 ≈ 707 > 200。
    let u0 = user_msg(&format!("u0{}", "x".repeat(400)));
    let a0 = assistant_tool_call("c0", "tool");
    let t0 = tool_result("c0", "tool", &"y".repeat(400));
    let u1 = user_msg(&format!("u1{}", "x".repeat(400)));
    let a1 = assistant_tool_call("c1", "tool");
    let t1 = tool_result("c1", "tool", &"y".repeat(400));
    let u2 = user_msg(&format!("u2{}", "x".repeat(400)));
    let msgs = vec![u0, a0, t0, u1.clone(), a1, t1, u2.clone()];

    let out = plan_context(&msgs, &policy, compactor.as_ref(), CancellationToken::new())
        .await
        .expect("should succeed");
    // keep_recent = 2：保留最近 2 个 turn（Turn 2 + Turn 3 = 4 条消息）。
    // 压缩 Turn 1（3 条消息）。
    let commit = out.commit.expect("should have commit");
    assert_eq!(commit.keep_from, 3, "keep_from 应为 Turn 1 的 3 条消息");
    // request_messages = [摘要] ++ [u1, a1, t1, u2]。
    assert_eq!(out.request_messages.len(), 5, "摘要 + 保留 4 条");
    assert_eq!(out.request_messages[1], u1, "保留 Turn 2 的第一条");
    assert_eq!(out.request_messages[4], u2, "保留 Turn 3 的第一条");
    // compactor 收到的是 Turn 1（3 条消息）。
    let compacted = compactor.last_compacted();
    assert_eq!(compacted.len(), 3, "应压缩 Turn 1 的 3 条消息");
}
