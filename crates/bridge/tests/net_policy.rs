//! The request policy, from the service's side - and the guard that keeps the two copies of it
//! identical.
//!
//! The policy lives in two files (`src/net.rs` in the CLI, `src/net.rs` here) because the CLI must
//! build *without* this crate: that is what the `service` feature exists for. The first test below
//! is what makes the duplication safe rather than a slow divergence - the two files are compared
//! byte for byte, so a change to one and not the other is a failing gate rather than a service that
//! retries where the CLI does not.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use linear_bridge::http_client::{HttpClient, Method, Request};

/// What one accepted connection does.
enum Script {
    /// Answer with a status and a JSON body.
    Answer(u16, &'static str),
    /// Accept and then say nothing, longer than any deadline in these tests.
    Stall,
}

/// A server that answers the first connections from `script`, on an ephemeral port.
fn serve(script: Vec<Script>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");

    std::thread::spawn(move || {
        for step in script {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = [0u8; 4096];
            let _ = stream.read(&mut head);

            match step {
                Script::Stall => std::thread::sleep(Duration::from_secs(30)),
                Script::Answer(status, body) => {
                    let response = format!(
                        "HTTP/1.1 {status} Something\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        }
    });

    format!("http://{address}/graphql")
}

fn request(url: String, repeatable: bool) -> Request {
    Request {
        method: Method::Get,
        url,
        headers: Vec::new(),
        body: None,
        repeatable,
    }
}

#[test]
fn the_cli_and_the_service_cannot_disagree_about_the_policy() {
    let cli = include_str!("../../../src/net.rs");
    let service = include_str!("../src/net.rs");

    assert_eq!(
        cli, service,
        "the two copies of the request policy have drifted apart. They are copies because the CLI \
         must build without this crate, and they are compared here because two hand-kept sets of \
         numbers is how the CLI and the service end up disagreeing about what a retry means. Edit \
         both - or neither."
    );
}

#[test]
fn a_repeatable_read_is_retried_and_a_write_is_not() {
    // Both cases get the same script: a 500 and then a proper answer. Sent twice, the request
    // succeeds - so the write, which must be sent once, is the one that reports the 500.
    let client = HttpClient::new();

    let read = serve(vec![
        Script::Answer(500, "{}"),
        Script::Answer(200, "{\"ok\":true}"),
    ]);
    let response = client
        .send(&request(read, true))
        .expect("a read is safe to repeat, so the 500 should have been retried away");
    assert_eq!(response.status, 200);

    let write = serve(vec![
        Script::Answer(500, "{}"),
        Script::Answer(200, "{\"ok\":true}"),
    ]);
    let response = client
        .send(&request(write, false))
        .expect("a 500 is a response, not a transport failure");
    assert_eq!(
        response.status, 500,
        "a request that is not safe to repeat must be sent exactly once"
    );
}

#[test]
fn a_hanging_upstream_is_a_bounded_failure() {
    // The deadline is a second; the server says nothing for thirty. Three attempts of a one-second
    // deadline is the bounded failure being asserted.
    std::env::set_var("LINEAR_REQUEST_TIMEOUT_SECS", "1");
    let url = serve(vec![
        Script::Stall,
        Script::Stall,
        Script::Stall,
        Script::Stall,
    ]);

    let started = Instant::now();
    let result = HttpClient::new().send(&request(url, true));
    let elapsed = started.elapsed();

    assert!(result.is_err(), "a stalled upstream is a failure");
    assert!(
        elapsed < Duration::from_secs(15),
        "the deadline should have bitten: took {elapsed:?}"
    );
}
