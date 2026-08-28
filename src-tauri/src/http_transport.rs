use std::{
    fmt,
    io::Read,
    sync::LazyLock,
    time::{Duration, Instant},
};
use ureq::Agent;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_IDLE_CONNECTIONS_PER_HOST: usize = 4;
const MAX_TRANSPORT_ATTEMPTS: usize = 3;

static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .max_idle_connections_per_host(MAX_IDLE_CONNECTIONS_PER_HOST)
        .max_idle_connections(MAX_IDLE_CONNECTIONS_PER_HOST * 2)
        .max_idle_age(IDLE_CONNECTION_TIMEOUT)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .build()
        .new_agent()
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    // Provider dashboards expose reads as POST RPCs, so bounded retries stay idempotent.
    PostRead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpErrorKind {
    Deadline,
    Transport,
    Oversized,
    InvalidRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub kind: HttpErrorKind,
    context: &'static str,
}

impl HttpError {
    fn new(kind: HttpErrorKind, context: &'static str) -> Self {
        Self { kind, context }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.context)
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub fn execute(
    method: HttpMethod,
    url: &str,
    headers: &[(&'static str, &str)],
    body: Option<&[u8]>,
    deadline: Instant,
    response_cap: usize,
    context: &'static str,
) -> Result<HttpResponse, HttpError> {
    if response_cap == 0 {
        return Err(HttpError::new(HttpErrorKind::InvalidRequest, context));
    }
    let mut attempts = 0usize;
    let mut response = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HttpError::new(HttpErrorKind::Deadline, context));
        }
        attempts += 1;
        let response = match method {
            HttpMethod::Get => {
                let mut request = AGENT.get(url);
                for (name, value) in headers {
                    request = request.header(*name, *value);
                }
                request
                    .config()
                    .timeout_global(Some(remaining))
                    .build()
                    .call()
            }
            HttpMethod::PostRead => {
                let mut request = AGENT.post(url);
                for (name, value) in headers {
                    request = request.header(*name, *value);
                }
                request
                    .config()
                    .timeout_global(Some(remaining))
                    .build()
                    .send(body.unwrap_or_default())
            }
        };
        match response {
            Ok(response) => break response,
            Err(error)
                if attempts < MAX_TRANSPORT_ATTEMPTS
                    && transport_error_is_retryable(&error)
                    && Instant::now() < deadline =>
            {
                continue;
            }
            Err(error) => {
                let kind = match error {
                    ureq::Error::Timeout(_) => HttpErrorKind::Deadline,
                    ureq::Error::BodyExceedsLimit(_) => HttpErrorKind::Oversized,
                    ureq::Error::Http(_) | ureq::Error::BadUri(_) => HttpErrorKind::InvalidRequest,
                    _ if Instant::now() >= deadline => HttpErrorKind::Deadline,
                    _ => HttpErrorKind::Transport,
                };
                return Err(HttpError::new(kind, context));
            }
        }
    };
    if response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > response_cap as u64)
    {
        return Err(HttpError::new(HttpErrorKind::Oversized, context));
    }
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut body = Vec::with_capacity(response_cap.min(64 * 1024));
    response
        .body_mut()
        .as_reader()
        .take(response_cap as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|_| {
            let kind = if Instant::now() >= deadline {
                HttpErrorKind::Deadline
            } else {
                HttpErrorKind::Transport
            };
            HttpError::new(kind, context)
        })?;
    if body.len() > response_cap {
        return Err(HttpError::new(HttpErrorKind::Oversized, context));
    }
    if Instant::now() >= deadline {
        return Err(HttpError::new(HttpErrorKind::Deadline, context));
    }
    Ok(HttpResponse {
        status,
        content_type,
        body,
    })
}

fn transport_error_is_retryable(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Timeout(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => true,
        ureq::Error::Io(error) => matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::TimedOut
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn fixture_server(body: &'static [u8]) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
            request
        });
        (format!("http://{address}"), worker)
    }

    #[test]
    fn bounded_transport_reads_json_without_exposing_authorization() {
        let (url, worker) = fixture_server(br#"{"ok":true}"#);
        let response = execute(
            HttpMethod::Get,
            &url,
            &[("authorization", "Bearer fixture-secret")],
            None,
            Instant::now() + Duration::from_secs(2),
            1024,
            "fixture request failed",
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, br#"{"ok":true}"#);
        let request = worker.join().unwrap();
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-secret"));
        assert!(!format!("{:?}", response).contains("fixture-secret"));
    }

    #[test]
    fn bounded_transport_rejects_declared_oversize() {
        let (url, worker) = fixture_server(b"oversized");
        let error = execute(
            HttpMethod::Get,
            &url,
            &[],
            None,
            Instant::now() + Duration::from_secs(2),
            4,
            "fixture request failed",
        )
        .unwrap_err();
        assert_eq!(error.kind, HttpErrorKind::Oversized);
        worker.join().unwrap();
    }

    #[test]
    fn process_wide_client_reuses_one_http_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut stream = std::io::BufReader::new(stream);
            for _ in 0..2 {
                loop {
                    let mut line = String::new();
                    assert!(std::io::BufRead::read_line(&mut stream, &mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                let body = br#"{"ok":true}"#;
                write!(
                    stream.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.get_mut().write_all(body).unwrap();
                stream.get_mut().flush().unwrap();
            }
        });

        for _ in 0..2 {
            let response = execute(
                HttpMethod::Get,
                &url,
                &[],
                None,
                Instant::now() + Duration::from_secs(2),
                1024,
                "fixture request failed",
            )
            .unwrap();
            assert_eq!(response.status, 200);
        }
        worker.join().unwrap();
    }

    #[test]
    #[ignore = "requires internet access"]
    fn live_https_transport_probe() {
        let response = execute(
            HttpMethod::Get,
            "https://github.com/",
            &[],
            None,
            Instant::now() + Duration::from_secs(8),
            2 * 1024 * 1024,
            "HTTPS fixture failed",
        )
        .unwrap();
        assert_eq!(response.status, 200);
    }

    #[test]
    fn retry_policy_is_limited_to_safe_transport_failures() {
        assert!(transport_error_is_retryable(&ureq::Error::Timeout(
            ureq::Timeout::Connect
        )));
        assert!(transport_error_is_retryable(&ureq::Error::ConnectionFailed));
        assert!(!transport_error_is_retryable(&ureq::Error::StatusCode(500)));
        assert!(!transport_error_is_retryable(&ureq::Error::Tls(
            "fixture TLS error"
        )));
    }
}
