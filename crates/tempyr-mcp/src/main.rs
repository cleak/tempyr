mod handler;
mod protocol;

use std::io::{self, BufRead, Write};

use protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use serde_json::{Value, json};

/// Wire format detected from the first client message.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Transport {
    /// Standard MCP: `Content-Length: N\r\n\r\n{json}`
    ContentLength,
    /// Legacy: one JSON object per line
    JsonLines,
}

fn main() {
    eprintln!("tempyr-mcp: MCP server starting on stdio");

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut transport = None;

    loop {
        let (message, detected) = match read_message(&mut reader) {
            Ok(Some(pair)) => pair,
            Ok(None) => break,
            Err(e) => {
                eprintln!("Error reading stdin: {e}");
                break;
            }
        };

        // Lock transport on first message
        let t = *transport.get_or_insert(detected);

        let outcome = process_request(&message);

        if let Some(response) = outcome.response {
            if let Err(e) = write_message(&mut writer, &response, t) {
                eprintln!("Error writing stdout: {e}");
                break;
            }
        }

        if outcome.shutdown {
            break;
        }
    }
}

fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<(String, Transport)>> {
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;

        if bytes_read == 0 {
            return Ok(None);
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }

        // Legacy JSON-lines: line starts with '{'
        if trimmed.starts_with('{') {
            return Ok(Some((trimmed.to_string(), Transport::JsonLines)));
        }

        // Standard MCP: Content-Length headers followed by body
        let mut content_length = None;
        parse_header(trimmed, &mut content_length)?;

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line)?;
            if bytes_read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Unexpected EOF while reading MCP headers",
                ));
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }

            parse_header(trimmed, &mut content_length)?;
        }

        let content_length = content_length.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Missing Content-Length header")
        })?;

        let mut body = vec![0_u8; content_length];
        reader.read_exact(&mut body)?;
        let body =
            String::from_utf8(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        return Ok(Some((body, Transport::ContentLength)));
    }
}

fn parse_header(line: &str, content_length: &mut Option<usize>) -> io::Result<()> {
    let (name, value) = line.split_once(':').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Malformed MCP header: {line}"),
        )
    })?;

    if name.eq_ignore_ascii_case("Content-Length") {
        *content_length = Some(value.trim().parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid Content-Length value '{value}': {e}"),
            )
        })?);
    }

    Ok(())
}

fn write_message<W: Write>(
    writer: &mut W,
    response: &JsonRpcResponse,
    transport: Transport,
) -> io::Result<()> {
    let body =
        serde_json::to_vec(response).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    match transport {
        Transport::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
            writer.write_all(&body)?;
        }
        Transport::JsonLines => {
            writer.write_all(&body)?;
            writer.write_all(b"\n")?;
        }
    }
    writer.flush()
}

#[derive(Debug)]
struct ProcessOutcome {
    response: Option<JsonRpcResponse>,
    shutdown: bool,
}

impl ProcessOutcome {
    fn with_response(response: JsonRpcResponse) -> Self {
        Self {
            response: Some(response),
            shutdown: false,
        }
    }

    fn notification() -> Self {
        Self {
            response: None,
            shutdown: false,
        }
    }

    fn shutdown(response: Option<JsonRpcResponse>) -> Self {
        Self {
            response,
            shutdown: true,
        }
    }
}

fn process_request(message: &str) -> ProcessOutcome {
    let request: JsonRpcRequest = match serde_json::from_str(message) {
        Ok(request) => request,
        Err(e) => {
            return ProcessOutcome::with_response(JsonRpcResponse::error(
                Value::Null,
                JsonRpcError {
                    code: -32700,
                    message: format!("Parse error: {e}"),
                    data: None,
                },
            ));
        }
    };

    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => {
            ProcessOutcome::with_response(handler::handle_initialize(id.unwrap_or(Value::Null)))
        }
        "initialized" | "notifications/initialized" => ProcessOutcome::notification(),
        "ping" => ProcessOutcome::with_response(JsonRpcResponse::success(
            id.unwrap_or(Value::Null),
            json!({}),
        )),
        "resources/list" => ProcessOutcome::with_response(JsonRpcResponse::success(
            id.unwrap_or(Value::Null),
            json!({ "resources": [] }),
        )),
        "resources/templates/list" => ProcessOutcome::with_response(JsonRpcResponse::success(
            id.unwrap_or(Value::Null),
            json!({ "resourceTemplates": [] }),
        )),
        "tools/list" => {
            ProcessOutcome::with_response(handler::handle_tools_list(id.unwrap_or(Value::Null)))
        }
        "tools/call" => ProcessOutcome::with_response(handler::handle_tool_call(
            id.unwrap_or(Value::Null),
            request.params,
        )),
        "shutdown" => {
            eprintln!("tempyr-mcp: shutdown requested");
            ProcessOutcome::shutdown(Some(JsonRpcResponse::success(
                id.unwrap_or(Value::Null),
                json!(null),
            )))
        }
        _ => ProcessOutcome::with_response(JsonRpcResponse::error(
            id.unwrap_or(Value::Null),
            JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
                data: None,
            },
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{Transport, process_request, read_message, write_message};
    use crate::protocol::JsonRpcResponse;
    use serde_json::{Value, json};
    use std::io::Cursor;

    #[test]
    fn read_message_detects_content_length_transport() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let framed = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let mut cursor = Cursor::new(framed.into_bytes());

        let (message, transport) = read_message(&mut cursor)
            .expect("frame should parse")
            .expect("should have a message");

        assert_eq!(message, body);
        assert_eq!(transport, Transport::ContentLength);
    }

    #[test]
    fn read_message_detects_json_lines_transport() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let mut cursor = Cursor::new(format!("{body}\n").into_bytes());

        let (message, transport) = read_message(&mut cursor)
            .expect("legacy line should parse")
            .expect("should have a message");

        assert_eq!(message, body);
        assert_eq!(transport, Transport::JsonLines);
    }

    #[test]
    fn write_message_content_length_format() {
        let response = JsonRpcResponse::success(Value::from(7), json!({"ok": true}));
        let mut output = Vec::new();

        write_message(&mut output, &response, Transport::ContentLength)
            .expect("frame should write");

        let rendered = String::from_utf8(output).expect("frame should be utf-8");
        assert!(rendered.starts_with("Content-Length: "));
        assert!(rendered.contains("\r\n\r\n"));
        assert!(rendered.ends_with(r#"{"jsonrpc":"2.0","result":{"ok":true},"id":7}"#));
    }

    #[test]
    fn write_message_json_lines_format() {
        let response = JsonRpcResponse::success(Value::from(7), json!({"ok": true}));
        let mut output = Vec::new();

        write_message(&mut output, &response, Transport::JsonLines).expect("line should write");

        let rendered = String::from_utf8(output).expect("line should be utf-8");
        assert!(!rendered.contains("Content-Length"));
        assert_eq!(
            rendered,
            "{\"jsonrpc\":\"2.0\",\"result\":{\"ok\":true},\"id\":7}\n"
        );
    }

    #[test]
    fn initialized_notification_does_not_emit_a_response() {
        let outcome = process_request(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
        );

        assert!(outcome.response.is_none());
        assert!(!outcome.shutdown);
    }

    #[test]
    fn resources_list_returns_empty_collection() {
        let outcome =
            process_request(r#"{"jsonrpc":"2.0","id":5,"method":"resources/list","params":{}}"#);

        let response = outcome.response.expect("resources/list should respond");
        let result = response
            .result
            .expect("resources/list should include a result");
        assert_eq!(result, json!({ "resources": [] }));
    }
}
