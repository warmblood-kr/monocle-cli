//! `/api/responses` turns can run paid tool calls and sub-agent delegations, so
//! a lost response must not be replayed by the transport (monocle-cli#172).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use monocle_cli::net::Client;
use monocle_cli::responses_api::ResponsesClient;

/// Accepts connections and drops them unanswered; returns base URL + accept count.
fn dropping_server() -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let accepts = Arc::new(AtomicUsize::new(0));
    let counter = accepts.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (url, accepts)
}

#[test]
fn respond_makes_a_single_attempt_on_transport_failure() {
    let (url, accepts) = dropping_server();
    let client = Client::new();
    let result = ResponsesClient::new(&client, "tok", url).respond("m", "hi", &[], &[], None, &[]);
    assert!(result.is_err());
    assert_eq!(accepts.load(Ordering::SeqCst), 1, "turn was replayed");
}
