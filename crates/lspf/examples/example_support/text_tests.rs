//! Example journeys use the same public transport seam as downstream handlers.
use bytes::Bytes;
use lspf::testing::ServerJourney;
use lspf::{RawMessage, RequestId, Server};
use serde_json::{Value, json};

pub(crate) const URI: &str = "file:///example.txt";

pub(crate) async fn opened<S: Send + Sync + 'static>(
    server: Server<S>,
    text: &str,
) -> ServerJourney {
    // Omitted position encodings negotiate UTF-16.
    let mut journey = ServerJourney::start(server).await.unwrap();
    journey
        .peer()
        .send(RawMessage::Notification {
            method: "textDocument/didOpen".into(),
            params: Bytes::from(
                serde_json::to_vec(&json!({"textDocument": {
                    "uri":URI,"languageId":"text","version":1,"text":text,
                }}))
                .unwrap(),
            ),
        })
        .unwrap();
    journey
}

pub(crate) async fn request(
    journey: &mut ServerJourney,
    method: &'static str,
    params: Value,
) -> Value {
    journey
        .peer()
        .send(RawMessage::Request {
            id: RequestId::Number(10),
            method: method.into(),
            params: Bytes::from(serde_json::to_vec(&params).unwrap()),
        })
        .unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), journey.peer().recv())
        .await
        .unwrap()
        .unwrap();
    let RawMessage::Response { id, result } = response else {
        panic!("unexpected response: {response:?}")
    };
    assert_eq!(id, RequestId::Number(10));
    serde_json::from_slice(&result.unwrap()).unwrap()
}
