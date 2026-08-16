use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::error::NetError;
use crate::types::SignalingMessage;

/// WebSocket client for communicating with the signaling server.
///
/// Handles connecting, sending, and receiving [`SignalingMessage`] values
/// serialized as JSON over a WebSocket connection.
pub struct SignalingClient {
    sink: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    stream: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl SignalingClient {
    /// Connect to a signaling server at the given WebSocket URL.
    pub async fn connect(url: &str) -> Result<Self, NetError> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
        let (sink, stream) = ws_stream.split();
        Ok(Self { sink, stream })
    }

    /// Send a signaling message to the server.
    pub async fn send(&mut self, msg: SignalingMessage) -> Result<(), NetError> {
        let json = serde_json::to_string(&msg)?;
        self.sink.send(Message::Text(json.into())).await?;
        Ok(())
    }

    /// Receive the next signaling message from the server.
    ///
    /// Returns `None` when the stream is closed. Skips Ping/Pong/Close frames.
    pub async fn recv(&mut self) -> Result<Option<SignalingMessage>, NetError> {
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    let msg: SignalingMessage = serde_json::from_str(&text)?;
                    return Ok(Some(msg));
                }
                Some(Ok(Message::Binary(data))) => {
                    let text = String::from_utf8_lossy(&data);
                    let msg: SignalingMessage = serde_json::from_str(&text)?;
                    return Ok(Some(msg));
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {
                    continue;
                }
                Some(Ok(Message::Close(_))) | None => {
                    return Ok(None);
                }
                Some(Err(e)) => {
                    return Err(NetError::WebSocket(e));
                }
            }
        }
    }

    /// Close the WebSocket connection.
    pub async fn close(&mut self) -> Result<(), NetError> {
        self.sink.close().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_to_invalid_url_returns_error() {
        let result = SignalingClient::connect("ws://127.0.0.1:19999").await;
        assert!(result.is_err());
    }
}
