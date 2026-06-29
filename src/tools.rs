use serde_json::Value;

use crate::config::Config;

pub async fn execute_tools(response: Value, _config: &Config) -> Value {
    // Phase 4: bubblewrap/wasmtime sandboxed tool execution
    // Parse tool_calls, execute in sandbox, inject results back
    response
}
