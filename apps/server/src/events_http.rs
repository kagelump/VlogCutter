//! The `/api/v1/ws/events` WebSocket.

use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SubscribeMessage {
    #[serde(rename = "type")]
    pub(super) msg_type: String,
    pub(super) project_id: String,
}

pub(super) async fn ws_events_handler(
    ws: WebSocketUpgrade,
    State(state): State<HttpState>,
    Extension(request_id): Extension<RequestId>,
    Extension(_principal): Extension<AuthPrincipal>,
    headers: HeaderMap,
) -> Response {
    if !headers.contains_key(header::ORIGIN) {
        return query_error_response(&request_id, "origin_mismatch", StatusCode::FORBIDDEN);
    }
    // Both browser sessions and device credentials receive non-canonical change
    // hints. Consumers must refetch the authenticated REST projection.
    ws.on_upgrade(move |socket| handle_ws_socket(socket, state))
}

pub(super) async fn handle_ws_socket(socket: WebSocket, state: HttpState) {
    let (mut sender, mut receiver) = socket.split();

    // First message MUST be a subscribe message:
    //   {"type":"subscribe","projectId":"proj_..."}
    let first_msg = match receiver.next().await {
        Some(Ok(WsMessage::Text(text))) => text.to_string(),
        _ => return,
    };

    let subscribe: SubscribeMessage = match serde_json::from_str::<SubscribeMessage>(&first_msg) {
        Ok(msg) if msg.msg_type == "subscribe" => msg,
        _ => return,
    };

    let mut rx = state.ws_channels.subscribe(&subscribe.project_id);

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(hint) => {
                        if sender.send(WsMessage::Text(hint.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            ws_msg = receiver.next() => {
                match ws_msg {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(WsMessage::Text(text))) => {
                        // Allow resubscribe to a different project
                        if let Ok(sub) = serde_json::from_str::<SubscribeMessage>(&text)
                            && sub.msg_type == "subscribe"
                        {
                            rx = state.ws_channels.subscribe(&sub.project_id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
