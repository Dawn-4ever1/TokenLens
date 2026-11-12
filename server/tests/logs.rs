//! Runs every route at TRACE level through a global subscriber and checks
//! that no credential or event content reaches the log.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{Harness, envelope, fixture_events, gzip, heartbeat};
use reqwest::StatusCode;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn credentials_and_event_content_stay_out_of_the_log() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("subscriber");

    let h = Harness::start().await;
    let first_code = h.issue_code();
    let first = h.pair_with(&first_code).await;
    let (_, body) = h
        .post("/api/pair/codes", Some(&first.read), Vec::new(), None)
        .await;
    let second_code = body["code"].as_str().expect("code").to_owned();
    let second = h.pair_with(&second_code).await;
    assert_eq!(
        h.claim("WXYZ-WXYZ-WXYZ").await.status(),
        StatusCode::UNAUTHORIZED
    );

    let events = fixture_events();
    let batch = envelope(&first.id, "batch-1", "g1", events.clone());
    let (status, _) = h.ingest(&first.ingest, &batch).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .post(
            "/api/ingest/events",
            Some(&second.ingest),
            gzip(
                &serde_json::to_vec(&envelope(&second.id, "batch-2", "g1", events.clone()))
                    .expect("encode"),
            ),
            Some("gzip"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .post(
            "/api/ingest/heartbeat",
            Some(&first.ingest),
            serde_json::to_vec(&heartbeat(&first.id)).expect("encode"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.ingest(&first.read, &batch).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let log = String::from_utf8(capture.0.lock().expect("lock").clone()).expect("utf8");
    assert!(log.contains("route=/api/ingest/events"), "{log}");
    assert!(log.contains("route=/api/pair/claim"));
    assert!(log.contains("status=401"));
    assert!(log.contains("batch stored"));

    let mut forbidden = vec![
        first.ingest.clone(),
        first.read.clone(),
        second.ingest.clone(),
        second.read.clone(),
        first_code.clone(),
        first_code.replace('-', ""),
        second_code.clone(),
        second_code.replace('-', ""),
        "WXYZ".to_owned(),
        "Bearer".to_owned(),
        "authorization".to_owned(),
        "Test Mac".to_owned(),
    ];
    for event in &events {
        for field in ["eventKey", "sessionId", "modelId"] {
            if let Some(text) = event[field].as_str() {
                forbidden.push(text.to_owned());
            }
        }
    }
    for needle in forbidden {
        assert!(
            !log.to_ascii_lowercase()
                .contains(&needle.to_ascii_lowercase()),
            "log contains a forbidden value"
        );
    }
}
