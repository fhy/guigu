//! Task 027 集成测试：derive 生成 schema 语义对齐 + Value↔RootSchema 往返。
//!
//! 仅 `schema` feature 下编译（helper 与 args 的 `JsonSchema` derive 均 feature-gated）。
//! 语义对齐 005/006 手工 JSON：断言 `type=object`、`properties` 键集合、`required`
//! 集合、数值约束（`minimum`）一致；`$schema`/`title` 等 schemars 附加字段允许存在，
//! 不逐字节比对。

#![cfg(feature = "schema")]

use std::collections::BTreeSet;

use guigu::tools::bash::BashArgs;
use guigu::tools::edit::EditArgs;
use guigu::tools::read::ReadArgs;
use guigu::tools::write::WriteArgs;
use guigu::{parameters, root_schema, schema_for};

/// 断言 object schema 语义：`type=object`、`properties` 键集合、`required` 集合。
fn assert_object_schema(
    params: &serde_json::Value,
    expected_props: &[&str],
    expected_required: &[&str],
) {
    assert_eq!(params["type"], "object", "should be an object schema");
    let props = params["properties"]
        .as_object()
        .expect("properties should be an object");
    let prop_keys: BTreeSet<&str> = props.keys().map(|s| s.as_str()).collect();
    let expected: BTreeSet<&str> = expected_props.iter().copied().collect();
    assert_eq!(prop_keys, expected, "properties keys should match");
    let required = params["required"]
        .as_array()
        .expect("required should be an array");
    let required_set: BTreeSet<&str> = required
        .iter()
        .map(|v| v.as_str().expect("required entry should be a string"))
        .collect();
    let expected_required_set: BTreeSet<&str> = expected_required.iter().copied().collect();
    assert_eq!(
        required_set, expected_required_set,
        "required set should match"
    );
}

/// read：schema 语义对齐 005 手工 JSON（path 必填；offset/limit 可选 + minimum）。
#[test]
fn test_read_schema_semantics() {
    let params = parameters::<ReadArgs>().expect("read parameters should be Some");
    assert_object_schema(&params, &["path", "offset", "limit"], &["path"]);
    // 数值约束对齐 005 手工 JSON（offset minimum=0，limit minimum=1）。
    // schemars 把 `minimum` 序列化为浮点（0.0/1.0），与手工 JSON 的整数（0/1）数值等价，
    // 故按数值比较（规格「约束一致、不逐字节比对」）。
    assert_eq!(
        params["properties"]["offset"]["minimum"].as_f64(),
        Some(0.0)
    );
    assert_eq!(params["properties"]["limit"]["minimum"].as_f64(), Some(1.0));
}

/// write：schema 语义对齐 005 手工 JSON（path/content 必填）。
#[test]
fn test_write_schema_semantics() {
    let params = parameters::<WriteArgs>().expect("write parameters should be Some");
    assert_object_schema(&params, &["path", "content"], &["path", "content"]);
}

/// edit：schema 语义对齐 005 手工 JSON（path/old_string/new_string 必填）。
#[test]
fn test_edit_schema_semantics() {
    let params = parameters::<EditArgs>().expect("edit parameters should be Some");
    assert_object_schema(
        &params,
        &["path", "old_string", "new_string"],
        &["path", "old_string", "new_string"],
    );
}

/// bash：schema 语义对齐 006 手工 JSON（command 必填；cwd/timeout_ms 可选 + minimum）。
#[test]
fn test_bash_schema_semantics() {
    let params = parameters::<BashArgs>().expect("bash parameters should be Some");
    assert_object_schema(&params, &["command", "cwd", "timeout_ms"], &["command"]);
    // 数值约束对齐 006 手工 JSON（timeout_ms minimum=1）。schemars 序列化为浮点，按数值比较。
    assert_eq!(
        params["properties"]["timeout_ms"]["minimum"].as_f64(),
        Some(1.0)
    );
}

/// Value↔RootSchema 往返：`parameters::<T>()` 的 Value 可被 `root_schema` 还原且一致。
#[test]
fn test_value_root_schema_roundtrip() {
    let params_list = [
        parameters::<ReadArgs>(),
        parameters::<WriteArgs>(),
        parameters::<EditArgs>(),
        parameters::<BashArgs>(),
    ];
    for params in params_list {
        let params = params.expect("parameters should be Some");
        let root = root_schema(Some(&params)).expect("root_schema should recover");
        let restored = serde_json::to_value(&root).expect("serialize restored");
        assert_eq!(restored, params, "round-trip should preserve the schema");
    }
}

/// `schema_for` 返回的 `RootSchema` 可 `to_value` 后再还原（Serialize/Deserialize 闭环）。
/// 经 `root_schema`（内部 `from_value`）反序列化，避免在集成测试直接命名 schemars 类型。
#[test]
fn test_schema_for_serialize_deserialize_closure() {
    let schema = schema_for::<ReadArgs>();
    let value = serde_json::to_value(&schema).expect("serialize");
    let restored = root_schema(Some(&value)).expect("deserialize");
    assert_eq!(
        serde_json::to_value(&schema).expect("serialize original"),
        serde_json::to_value(&restored).expect("serialize restored"),
        "Serialize/Deserialize closure should preserve the schema"
    );
}
