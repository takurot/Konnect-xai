use anyhow::Result;
use konnect_core::mcp::{handler::McpHandler, protocol::*};
use serde_json::Value;
use std::collections::VecDeque;
use std::future::Future;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{error, info};

const MAX_QUEUED_CALLS: usize = 128;
const MAX_CONSECUTIVE_READS: usize = 32;

pub enum StdioExit {
    Eof,
    #[cfg(unix)]
    Reload(konnect_core::router::meta_tools::ReloadPlan),
}

/// Run the MCP server over STDIO (stdin/stdout).
/// All logging must go to stderr — stdout is reserved for the MCP protocol.
pub async fn run_stdio(handler: McpHandler) -> Result<StdioExit> {
    info!("Starting STDIO transport");
    let dispatch_handler = handler.clone();
    run_stdio_io(
        handler,
        tokio::io::stdin(),
        tokio::io::stdout(),
        move |msg| {
            let handler = dispatch_handler.clone();
            async move { handler.handle_message(msg).await }
        },
    )
    .await
}

struct PendingCall {
    message: Value,
    cancelled: bool,
}

impl PendingCall {
    fn has_id(&self, id: &Value) -> bool {
        !self.cancelled && self.message.get("id") == Some(id)
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
    output: &mut W,
    response: &JsonRpcResponse,
) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    output.write_all(line.as_bytes()).await?;
    output.flush().await?;
    Ok(())
}

async fn flush_notifications<W: AsyncWrite + Unpin>(
    output: &mut W,
    notifications: &mut mpsc::Receiver<String>,
) -> Result<()> {
    while let Ok(mut line) = notifications.try_recv() {
        line.push('\n');
        output.write_all(line.as_bytes()).await?;
    }
    output.flush().await?;
    Ok(())
}

fn request_error(id: Value, message: &str) -> JsonRpcResponse {
    JsonRpcResponse::error(
        id,
        JsonRpcError {
            code: INVALID_REQUEST,
            message: message.into(),
            data: None,
        },
    )
}

/// One loop owns stdout and the FIFO of tool calls. Only the currently active
/// call gets a task: document mutations retain their original ordering, while
/// ping and cancellation remain responsive. Dropping/aborting that task drops
/// its owned CLI/Java children. A tool panic cannot unwind the session loop.
async fn run_stdio_io<R, W, F, Fut>(
    handler: McpHandler,
    input: R,
    mut output: W,
    dispatch: F,
) -> Result<StdioExit>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: Fn(Value) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Option<JsonRpcResponse>> + Send + 'static,
{
    let mut lines = BufReader::new(input).lines();
    let (notif_tx, mut notifications) = mpsc::channel::<String>(16);
    handler.register_notification_sink(notif_tx).await;
    let mut calls = VecDeque::<PendingCall>::new();
    let mut active: Option<PendingCall> = None;
    let mut tasks = JoinSet::new();
    let mut eof = false;
    let mut consecutive_reads = 0;
    enum Event {
        Input(std::io::Result<Option<String>>),
        Completed(Option<Result<Option<JsonRpcResponse>, tokio::task::JoinError>>),
    }

    loop {
        if active.is_none() {
            if let Some(call) = calls.pop_front() {
                let dispatch = dispatch.clone();
                let message = call.message.clone();
                tasks.spawn(async move { dispatch(message).await });
                active = Some(call);
            } else if eof {
                return Ok(StdioExit::Eof);
            }
        }

        // Prefer cancellation briefly, but reap ready completions regularly even
        // while clients continuously send ping or notification traffic.
        let ready = if consecutive_reads >= MAX_CONSECUTIVE_READS {
            consecutive_reads = 0;
            tasks.try_join_next()
        } else {
            None
        };
        let event = if let Some(completed) = ready {
            Event::Completed(Some(completed))
        } else {
            tokio::select! {
                biased;
                line = lines.next_line(), if !eof => Event::Input(line),
                completed = tasks.join_next(), if active.is_some() => Event::Completed(completed),
            }
        };
        match event {
            Event::Input(line) => {
                consecutive_reads += 1;
                let Some(line) = line? else {
                    // A piped client may close stdin after its final request.
                    // Finish accepted calls and flush their responses first.
                    eof = true;
                    continue;
                };
                if line.trim().is_empty() {
                    continue;
                }
                let message = match serde_json::from_str::<Value>(&line) {
                    Ok(message) => message,
                    Err(error) => {
                        write_response(
                            &mut output,
                            &JsonRpcResponse::error(
                                Value::Null,
                                JsonRpcError {
                                    code: PARSE_ERROR,
                                    message: format!("Parse error: {error}"),
                                    data: None,
                                },
                            ),
                        )
                        .await?;
                        continue;
                    }
                };
                let method = message.get("method").and_then(Value::as_str);
                if method == Some("notifications/cancelled") && message.get("id").is_none() {
                    if let Some(id) = message
                        .get("params")
                        .and_then(|params| params.get("requestId"))
                    {
                        calls.retain(|call| !call.has_id(id));
                        if let Some(call) = active.as_mut().filter(|call| call.has_id(id)) {
                            call.cancelled = true;
                            tasks.abort_all();
                        }
                    }
                    continue;
                }
                if method == Some("tools/call") {
                    if let Some(id) = message.get("id") {
                        let duplicate = active.as_ref().is_some_and(|call| call.has_id(id))
                            || calls.iter().any(|call| call.has_id(id));
                        if duplicate || calls.len() >= MAX_QUEUED_CALLS {
                            let reason = if duplicate {
                                "Request id is already in flight"
                            } else {
                                "Too many queued tool calls"
                            };
                            write_response(&mut output, &request_error(id.clone(), reason)).await?;
                            continue;
                        }
                    } else if calls.len() >= MAX_QUEUED_CALLS {
                        continue;
                    }
                    calls.push_back(PendingCall {
                        message,
                        cancelled: false,
                    });
                    continue;
                }
                // Lifecycle and cheap protocol requests never wait behind tools.
                let expects_response = message.get("id").is_some();
                if let Some(response) = dispatch(message).await.filter(|_| expects_response) {
                    write_response(&mut output, &response).await?;
                }
                if active.is_none() {
                    flush_notifications(&mut output, &mut notifications).await?;
                }
            }
            Event::Completed(completed) => {
                consecutive_reads = 0;
                let call = active.take().expect("only active calls have tasks");
                let response = match completed.expect("active call owns a task") {
                    Ok(response) => response,
                    Err(error) if error.is_cancelled() => None,
                    Err(_) => {
                        // Panic payloads can contain user file data or secrets.
                        error!(
                            tool = call.message["params"]["name"].as_str().unwrap_or("unknown"),
                            "STDIO tool handler panicked"
                        );
                        Some(JsonRpcResponse::error(
                            call.message["id"].clone(),
                            JsonRpcError {
                                code: INTERNAL_ERROR,
                                message:
                                    "Tool handler panicked; inspect document state before retrying"
                                        .into(),
                                data: None,
                            },
                        ))
                    }
                };
                let mut response_flushed = false;
                if !call.cancelled && call.message.get("id").is_some() {
                    if let Some(response) = response {
                        write_response(&mut output, &response).await?;
                        response_flushed = true;
                    }
                }
                // Preserve response-before-notification for load/unload toolset.
                flush_notifications(&mut output, &mut notifications).await?;
                if let Some(plan) = handler.take_reload_request() {
                    #[cfg(unix)]
                    if response_flushed {
                        tracing::debug!("STDIO reload response flushed");
                        return Ok(StdioExit::Reload(plan));
                    }
                    // A cancelled request must never reload without its reply.
                    let _ = (plan, response_flushed);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Notify;
    use tokio::time::{timeout, Duration};

    async fn response(
        reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    ) -> Value {
        let mut line = String::new();
        timeout(Duration::from_secs(3), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).expect("session must return a response")
    }

    async fn send(writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>, msg: Value) {
        writer
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stdio_handler_panic_isolated_and_next_request_succeeds() {
        let handler = McpHandler::new(Default::default()).await.unwrap();
        let (client, server) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(server);
        let session = tokio::spawn(run_stdio_io(
            handler,
            input,
            output,
            |msg: Value| async move {
                if msg["method"] == "tools/call" {
                    panic!("fixture panic");
                }
                Some(JsonRpcResponse::success(msg["id"].clone(), json!({})))
            },
        ));
        let (input, mut output) = tokio::io::split(client);
        let mut reader = BufReader::new(input);
        send(
            &mut output,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"panic_fixture"}}),
        )
        .await;
        let failed = response(&mut reader).await;
        assert_eq!(failed["id"], 1);
        assert_eq!(failed["error"]["code"], INTERNAL_ERROR);
        send(&mut output, json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
        assert_eq!(response(&mut reader).await["id"], 2);
        output.shutdown().await.unwrap();
        assert!(session.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn stdio_ping_overtakes_slow_call_and_cancel_suppresses_response() {
        let handler = McpHandler::new(Default::default()).await.unwrap();
        let (client, server) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(server);
        let started = Arc::new(Notify::new());
        let dispatch_started = started.clone();
        let session = tokio::spawn(run_stdio_io(handler, input, output, move |msg: Value| {
            let started = dispatch_started.clone();
            async move {
                if msg["method"] == "tools/call" {
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
                Some(JsonRpcResponse::success(msg["id"].clone(), json!({})))
            }
        }));
        let (input, mut output) = tokio::io::split(client);
        let mut reader = BufReader::new(input);
        send(
            &mut output,
            json!({"jsonrpc":"2.0","id":"slow","method":"tools/call"}),
        )
        .await;
        timeout(Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        send(&mut output, json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
        assert_eq!(response(&mut reader).await["id"], 2);
        send(&mut output, json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"slow"}})).await;
        send(&mut output, json!({"jsonrpc":"2.0","id":3,"method":"ping"})).await;
        assert_eq!(response(&mut reader).await["id"], 3);
        output.shutdown().await.unwrap();
        assert!(timeout(Duration::from_secs(3), session)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        let mut remaining = String::new();
        reader.read_line(&mut remaining).await.unwrap();
        assert!(
            remaining.is_empty(),
            "cancelled request emitted {remaining}"
        );
    }
    #[tokio::test]
    async fn stdio_ready_input_does_not_starve_completed_tool_response() {
        let handler = McpHandler::new(Default::default()).await.unwrap();
        let (client, server) = tokio::io::duplex(1024 * 1024);
        let (input, output) = tokio::io::split(server);
        let session = tokio::spawn(run_stdio_io(
            handler,
            input,
            output,
            |msg: Value| async move { Some(JsonRpcResponse::success(msg["id"].clone(), json!({}))) },
        ));
        let (input, mut output) = tokio::io::split(client);
        let mut reader = BufReader::new(input);
        // Queue input in one write so stdin remains ready after tool completion.
        let mut requests = format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":"tool","method":"tools/call"})
        );
        for id in 0..4096 {
            requests.push_str(&format!(
                "{}\n",
                json!({"jsonrpc":"2.0","id":id,"method":"ping"})
            ));
        }
        output.write_all(requests.as_bytes()).await.unwrap();
        let mut before_tool = 0;
        loop {
            if response(&mut reader).await["id"] == "tool" {
                break;
            }
            before_tool += 1;
        }
        assert!(
            before_tool < 4096,
            "ready stdin starved tool until every ping was read"
        );
        output.shutdown().await.unwrap();
        assert!(session.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn stdio_queued_cancel_distinguishes_string_ids_and_eof_drains_accepted_calls() {
        let handler = McpHandler::new(Default::default()).await.unwrap();
        let (client, server) = tokio::io::duplex(8192);
        let (input, output) = tokio::io::split(server);
        let release = Arc::new(Notify::new());
        let dispatch_release = release.clone();
        let session = tokio::spawn(run_stdio_io(handler, input, output, move |msg: Value| {
            let release = dispatch_release.clone();
            async move {
                if msg["method"] == "tools/call" {
                    assert_eq!(msg["id"], 1, "cancelled queued request must never execute");
                    release.notified().await;
                }
                Some(JsonRpcResponse::success(msg["id"].clone(), json!({})))
            }
        }));
        let (input, mut output) = tokio::io::split(client);
        let mut reader = BufReader::new(input);
        send(
            &mut output,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call"}),
        )
        .await;
        send(
            &mut output,
            json!({"jsonrpc":"2.0","id":"1","method":"tools/call"}),
        )
        .await;
        send(
            &mut output,
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"1"}}),
        )
        .await;
        send(&mut output, json!({"jsonrpc":"2.0","id":2,"method":"ping"})).await;
        assert_eq!(response(&mut reader).await["id"], 2);
        release.notify_one();
        output.shutdown().await.unwrap();
        assert_eq!(
            response(&mut reader).await["id"],
            1,
            "EOF must drain the active call"
        );
        assert!(timeout(Duration::from_secs(3), session)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        let mut remaining = String::new();
        reader.read_line(&mut remaining).await.unwrap();
        assert!(remaining.is_empty());
    }
}
