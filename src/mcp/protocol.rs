use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The only JSON-RPC version accepted on the wire.
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC error code used for unsupported server-to-client requests.
pub const METHOD_NOT_FOUND_CODE: i64 = -32601;

/// Request identifiers are preserved as JSON values, but the harness itself
/// only generates and correlates numeric `u64` identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(u64),
    String(String),
}

impl JsonRpcId {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::String(_) => None,
        }
    }
}

impl From<u64> for JsonRpcId {
    fn from(value: u64) -> Self {
        Self::Number(value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl JsonRpcRequest {
    pub fn new(id: impl Into<JsonRpcId>, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            method: method.into(),
            params: None,
        }
    }

    pub fn with_params(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            method: method.into(),
            params: Some(params),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: impl Into<JsonRpcId>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(id.into()),
            result: Some(result),
            error: None,
        }
    }

    pub fn error_response(id: impl Into<JsonRpcId>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(id.into()),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// A syntactically valid inbound JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundMessage {
    Response(JsonRpcResponse),
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("malformed JSON: {0}")]
    MalformedJson(String),

    #[error("JSON-RPC message must be an object")]
    NotAnObject,

    #[error("unsupported JSON-RPC version `{0}`")]
    UnsupportedVersion(String),

    #[error("response is missing a valid id")]
    MissingResponseId,

    #[error("response must contain exactly one of `result` or `error`")]
    InvalidResponseShape,

    #[error("request id must be a non-negative integer or string")]
    InvalidId,

    #[error("field `{0}` must be a string")]
    InvalidStringField(&'static str),

    #[error("invalid JSON-RPC error object: {0}")]
    InvalidError(String),
}

/// Parse one newline-delimited JSON-RPC message and classify it strictly.
///
/// Malformed JSON, wrong versions, response shapes with both or neither
/// `result`/`error`, and invalid identifiers are protocol errors. Callers must
/// not silently ignore these while requests are pending.
pub fn parse_inbound(line: &str) -> Result<InboundMessage, ProtocolError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| ProtocolError::MalformedJson(error.to_string()))?;
    let object = value.as_object().ok_or(ProtocolError::NotAnObject)?;

    match object.get("jsonrpc") {
        Some(Value::String(version)) if version == JSONRPC_VERSION => {}
        Some(Value::String(version)) => {
            return Err(ProtocolError::UnsupportedVersion(version.clone()));
        }
        _ => return Err(ProtocolError::UnsupportedVersion("<missing>".into())),
    }

    let id = object.get("id").filter(|value| !value.is_null());
    let has_method = object.contains_key("method");

    if has_method {
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or(ProtocolError::InvalidStringField("method"))?
            .to_owned();
        let params = object.get("params").cloned();

        return match id {
            Some(id) => Ok(InboundMessage::Request(JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.into(),
                id: parse_id(id)?,
                method,
                params: params.unwrap_or(Value::Null),
            })),
            None => Ok(InboundMessage::Notification(JsonRpcNotification {
                jsonrpc: JSONRPC_VERSION.into(),
                method,
                params,
            })),
        };
    }

    let id = id.ok_or(ProtocolError::MissingResponseId)?;
    let id = parse_id(id)?;
    let result = object.get("result").cloned();
    let error = object
        .get("error")
        .map(|value| {
            serde_json::from_value::<JsonRpcError>(value.clone())
                .map_err(|error| ProtocolError::InvalidError(error.to_string()))
        })
        .transpose()?;

    if result.is_some() == error.is_some() {
        return Err(ProtocolError::InvalidResponseShape);
    }

    Ok(InboundMessage::Response(JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION.into(),
        id: Some(id),
        result,
        error,
    }))
}

fn parse_id(value: &Value) -> Result<JsonRpcId, ProtocolError> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .map(JsonRpcId::Number)
            .ok_or(ProtocolError::InvalidId),
        Value::String(value) => Ok(JsonRpcId::String(value.clone())),
        _ => Err(ProtocolError::InvalidId),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_serializes_as_json_rpc_2() {
        let request = JsonRpcRequest::new(7, "tools/call", json!({"name": "read_file"}));
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], "tools/call");
    }

    #[test]
    fn notification_omits_absent_params() {
        let notification = JsonRpcNotification::new("notifications/initialized");
        let value = serde_json::to_value(notification).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "notifications/initialized");
        assert!(value.get("params").is_none());
    }

    #[test]
    fn response_accepts_errors_without_results() {
        let response: JsonRpcResponse = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "error": {"code": -32601, "message": "method not found"}
        }))
        .unwrap();
        assert_eq!(response.id, Some(JsonRpcId::Number(3)));
        assert!(response.result.is_none());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[test]
    fn parses_responses_requests_and_notifications() {
        let response = parse_inbound(r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#).unwrap();
        assert!(matches!(response, InboundMessage::Response(_)));

        let request =
            parse_inbound(r#"{"jsonrpc":"2.0","id":9001,"method":"mock/ping","params":{}}"#)
                .unwrap();
        assert!(matches!(request, InboundMessage::Request(_)));

        let notification =
            parse_inbound(r#"{"jsonrpc":"2.0","method":"notifications/mock"}"#).unwrap();
        assert!(matches!(notification, InboundMessage::Notification(_)));
    }

    #[test]
    fn rejects_malformed_json_and_wrong_versions() {
        assert!(matches!(
            parse_inbound("{not json"),
            Err(ProtocolError::MalformedJson(_))
        ));
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"1.0","id":1,"result":null}"#),
            Err(ProtocolError::UnsupportedVersion(_))
        ));
        assert!(matches!(
            parse_inbound(r#"{"id":1,"result":null}"#),
            Err(ProtocolError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn rejects_malformed_response_shapes_and_ids() {
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","result":null}"#),
            Err(ProtocolError::MissingResponseId)
        ));
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","id":1}"#),
            Err(ProtocolError::InvalidResponseShape)
        ));
        assert!(matches!(
            parse_inbound(
                r#"{"jsonrpc":"2.0","id":1,"result":null,"error":{"code":1,"message":"x"}}"#
            ),
            Err(ProtocolError::InvalidResponseShape)
        ));
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","id":-1,"result":null}"#),
            Err(ProtocolError::InvalidId)
        ));
        assert!(matches!(
            parse_inbound(r#"{"jsonrpc":"2.0","id":{"bad":true},"result":null}"#),
            Err(ProtocolError::InvalidId)
        ));
    }

    #[test]
    fn parses_string_ids_for_server_requests() {
        let message =
            parse_inbound(r#"{"jsonrpc":"2.0","id":"server-1","method":"mock/ping","params":{}}"#)
                .unwrap();
        let InboundMessage::Request(request) = message else {
            panic!("expected request");
        };
        assert_eq!(request.id, JsonRpcId::String("server-1".into()));
        assert_eq!(request.id.as_u64(), None);
    }
}
