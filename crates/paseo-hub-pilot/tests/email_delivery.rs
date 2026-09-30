use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use paseo_hub_pilot::{HubError, InvitationEmailMessage, ResendConfig, ResendEmailDelivery};

#[test]
fn resend_configuration_is_optional_and_strict() {
    assert_eq!(
        ResendConfig::from_environment(&BTreeMap::new()).unwrap(),
        None
    );
    assert_eq!(
        ResendConfig::from_environment(&BTreeMap::from([(
            "RESEND_API_KEY".to_owned(),
            "   ".to_owned(),
        )]))
        .unwrap(),
        None
    );
    assert_eq!(
        ResendConfig::from_environment(&BTreeMap::from([
            ("RESEND_API_KEY".to_owned(), " re_test_abc123 ".to_owned()),
            (
                "RESEND_FROM".to_owned(),
                " Paseo <mail@example.com> ".to_owned(),
            ),
        ]))
        .unwrap(),
        Some(ResendConfig {
            api_key: "re_test_abc123".to_owned(),
            from: "Paseo <mail@example.com>".to_owned(),
        })
    );
    assert!(matches!(
        ResendConfig::from_environment(&BTreeMap::from([
            ("RESEND_API_KEY".to_owned(), "not-a-key".to_owned()),
            ("RESEND_FROM".to_owned(), "mail@example.com".to_owned()),
        ])),
        Err(HubError::EmailDeliveryConfig)
    ));
}

#[test]
fn resend_delivery_emits_exact_headers_and_json_without_exposing_failure_body() {
    let (endpoint, success) = capture_one_response(200, r#"{"id":"email-1"}"#);
    let delivery = ResendEmailDelivery::with_endpoint(
        ResendConfig {
            api_key: "re_test_abc123".to_owned(),
            from: "Paseo <mail@example.com>".to_owned(),
        },
        endpoint,
    )
    .unwrap();
    delivery.send(&message()).unwrap();
    let request = success.join().unwrap();
    assert!(request.starts_with("POST /emails HTTP/1.1\r\n"));
    assert!(request.contains("authorization: Bearer re_test_abc123\r\n"));
    assert!(request.contains("content-type: application/json\r\n"));
    assert!(request.contains("idempotency-key: paseo-invitation-invite-1\r\n"));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        serde_json::json!({
            "from": "Paseo <mail@example.com>",
            "to": ["person@example.com"],
            "subject": "Join Acme on Paseo",
            "text": "Text body",
            "html": "<p>HTML body</p>"
        })
    );

    let (endpoint, rejected) = capture_one_response(422, "secret provider detail");
    let delivery = ResendEmailDelivery::with_endpoint(
        ResendConfig {
            api_key: "re_test_abc123".to_owned(),
            from: "mail@example.com".to_owned(),
        },
        endpoint,
    )
    .unwrap();
    assert_eq!(
        delivery.send(&message()),
        Err(HubError::EmailDeliveryRejected(422))
    );
    rejected.join().unwrap();
}

fn message() -> InvitationEmailMessage {
    InvitationEmailMessage {
        to: "person@example.com".to_owned(),
        subject: "Join Acme on Paseo".to_owned(),
        text: "Text body".to_owned(),
        html: "<p>HTML body</p>".to_owned(),
        idempotency_key: "paseo-invitation-invite-1".to_owned(),
    }
}

fn capture_one_response(status: u16, response_body: &str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/emails", listener.local_addr().unwrap());
    let response_body = response_body.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            let Some((headers, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap();
            if body.len() >= content_length {
                break;
            }
        }
        let reason = if status == 200 {
            "OK"
        } else {
            "Unprocessable Content"
        };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        )
        .unwrap();
        String::from_utf8(request).unwrap()
    });
    (endpoint, handle)
}
