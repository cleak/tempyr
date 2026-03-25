mod handler;
mod protocol;

use std::io::{self, BufRead, Write};

use protocol::{JsonRpcRequest, JsonRpcResponse, JsonRpcError};

fn main() {
    eprintln!("tempyr-mcp: MCP server starting on stdio");

    let stdin = io::stdin();
    let stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Error reading stdin: {e}");
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let response = process_request(&line);
        let response_json = serde_json::to_string(&response).unwrap_or_else(|e| {
            format!(r#"{{"jsonrpc":"2.0","error":{{"code":-32603,"message":"Serialization error: {e}"}},"id":null}}"#)
        });

        let mut stdout = stdout.lock();
        let _ = writeln!(stdout, "{response_json}");
        let _ = stdout.flush();
    }
}

fn process_request(line: &str) -> JsonRpcResponse {
    let request: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return JsonRpcResponse::error(
                serde_json::Value::Null,
                JsonRpcError {
                    code: -32700,
                    message: format!("Parse error: {e}"),
                    data: None,
                },
            );
        }
    };

    let id = request.id.clone();

    match request.method.as_str() {
        "initialize" => handler::handle_initialize(id),
        "initialized" => JsonRpcResponse::success(id, serde_json::json!(null)),
        "tools/list" => handler::handle_tools_list(id),
        "tools/call" => handler::handle_tool_call(id, request.params),
        "shutdown" => {
            eprintln!("tempyr-mcp: shutdown requested");
            std::process::exit(0);
        }
        _ => JsonRpcResponse::error(
            id,
            JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
                data: None,
            },
        ),
    }
}
