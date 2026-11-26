//! Loopback HTTP fixture shared by the process-level tests.
#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type Responder = dyn Fn(&str) -> Vec<u8> + Send + Sync;

/// Serves one canned HTTP response per connection and records request paths.
pub struct Fixture {
    pub origin: String,
    pub requests: Arc<Mutex<Vec<(Instant, String)>>>,
}

impl Fixture {
    pub fn start(responder: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let responder: Arc<Responder> = Arc::new(responder);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap() <= 2 {
                        break;
                    }
                }
                let path = request_line
                    .split(' ')
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                recorded
                    .lock()
                    .unwrap()
                    .push((Instant::now(), path.clone()));
                let _ = stream.write_all(&responder(&path));
                // Close gracefully: dropping a socket right after a large write
                // can reset the connection before the client reads everything.
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = std::io::copy(&mut reader, &mut std::io::sink());
            }
        });
        Self { origin, requests }
    }

    pub fn paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect()
    }
}

pub fn response(status: &str, content_type: &str, extra_headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}
