//! 类型化工具参数 schema 辅助（Task 027）。仅 `schema` feature 下编译。
//!
//! 三个纯函数 helper，无 I/O、无 async：
//! - [`schema_for`]：从 `JsonSchema` 类型生成 `RootSchema`（薄封装 `schemars::schema_for!`）。
//! - [`parameters`]：供 `Tool::parameters()` 使用，生成 schema 并序列化为 `Value`。
//! - [`root_schema`]：把 `Tool::parameters()` 的 `Value` 反序列化回 `RootSchema`，
//!   供 ACP/插件/编辑器消费；旧手工 JSON 若不符合 `SchemaObject` 形状则返回 `None`（容错）。

use schemars::JsonSchema;

/// 从实现了 [`JsonSchema`] 的类型生成 JSON Schema（[`RootSchema`]）。
///
/// 薄封装 `schemars::schema_for!`，统一入口。
pub fn schema_for<T: JsonSchema>() -> schemars::schema::RootSchema {
    schemars::schema_for!(T)
}

/// 供 `Tool::parameters()` 使用：从类型 derive 生成 schema 并序列化为 `Value`。
///
/// 序列化理论不会失败（`RootSchema` 字段均 JSON 兼容），失败返回 `None`（无 unwrap）。
pub fn parameters<T: JsonSchema>() -> Option<serde_json::Value> {
    serde_json::to_value(schema_for::<T>()).ok()
}

/// 从 `Tool::parameters()` 的 `Value` 反序列化为类型化 [`RootSchema`]，
/// 供 ACP/插件/编辑器消费。
///
/// 旧手工 JSON 若不符合 schemars `SchemaObject` 形状则返回 `None`，调用方据此降级（容错）。
pub fn root_schema(params: Option<&serde_json::Value>) -> Option<schemars::schema::RootSchema> {
    params.and_then(|v| serde_json::from_value(v.clone()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    /// 测试用参数类型：`name` 必填、`count` 可选（非 Option → required，Option → 非 required）。
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct TestArgs {
        name: String,
        count: Option<u64>,
    }

    /// `schema_for` 生成的 `RootSchema` 可 `to_value` 后再 `from_value` 还原（Serialize/Deserialize 闭环）。
    #[test]
    fn test_schema_for_roundtrip() {
        let schema = schema_for::<TestArgs>();
        let value = serde_json::to_value(&schema).expect("RootSchema should serialize");
        let restored: schemars::schema::RootSchema =
            serde_json::from_value(value).expect("RootSchema should deserialize");
        assert_eq!(
            serde_json::to_value(&schema).expect("serialize original"),
            serde_json::to_value(&restored).expect("serialize restored"),
            "round-trip should preserve the schema"
        );
    }

    /// `schema_for` 生成的 object schema：`name` 进 required，`count`（Option）不进。
    /// `RootSchema` 经 `#[serde(flatten)]` 序列化后 `type`/`required` 在顶层。
    #[test]
    fn test_schema_for_required_from_non_option() {
        let value = serde_json::to_value(schema_for::<TestArgs>()).expect("serialize");
        assert_eq!(value["type"], "object");
        let required = value["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&serde_json::json!("name")));
        assert!(!required.contains(&serde_json::json!("count")));
    }

    /// `parameters` 返回 `Some(Value)`，且可被 `root_schema` 反序列化回 `RootSchema`（往返一致）。
    #[test]
    fn test_parameters_roundtrip_via_root_schema() {
        let params = parameters::<TestArgs>();
        assert!(params.is_some(), "parameters should be Some");
        let root = root_schema(params.as_ref());
        assert!(root.is_some(), "root_schema should recover the RootSchema");
        // 往返一致：root_schema 还原的 schema 与原始序列化值等价。
        let restored_value =
            serde_json::to_value(root.expect("checked is_some")).expect("serialize restored");
        assert_eq!(restored_value, params.expect("checked is_some"));
    }

    /// `root_schema(None)` 返回 `None`（容错不 panic）。
    #[test]
    fn test_root_schema_none() {
        assert!(root_schema(None).is_none());
    }

    /// `root_schema(非法 JSON)` 返回 `None`（容错不 panic）。
    /// 字符串/数组既非 bool 也非 object，无法反序列化为 `RootSchema`。
    #[test]
    fn test_root_schema_invalid_returns_none() {
        let not_a_schema = serde_json::json!("this is not a schema");
        assert!(root_schema(Some(&not_a_schema)).is_none());
        let array = serde_json::json!([1, 2, 3]);
        assert!(root_schema(Some(&array)).is_none());
    }

    /// 旧手工 JSON（合法 `SchemaObject` 形状）可被 `root_schema` 解析（向后兼容）。
    #[test]
    fn test_root_schema_accepts_legacy_manual_json() {
        let legacy = serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        });
        assert!(root_schema(Some(&legacy)).is_some());
    }
}
