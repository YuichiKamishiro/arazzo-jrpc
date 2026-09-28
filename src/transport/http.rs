//! Built-in HTTP transport over `reqwest::blocking` (feature `http`).

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::Method;

use super::{HttpRequest, HttpResponse, HttpTransport, TransportError};

/// Blocking HTTP client for OpenAPI steps; one connection pool for all sends.
#[derive(Default)]
pub struct ReqwestHttp {
    // The client is created on the first request: Default cannot return an error.
    client: Option<Client>,
}

impl HttpTransport for ReqwestHttp {
    /// Sends `request` and reads the whole response body.
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let fail = |what: &str, e: &dyn std::fmt::Display| TransportError::new(format!("{} {}: {what}: {e}", request.method, request.url));
        let client = match &self.client {
            Some(c) => c.clone(),
            None => {
                // No global timeout: it is set per request.
                let c = Client::builder().timeout(None::<Duration>).build().map_err(|e| fail("cannot create HTTP client", &e))?;
                self.client.insert(c).clone()
            }
        };
        let method = Method::from_bytes(request.method.as_bytes()).map_err(|e| fail("invalid HTTP method", &e))?;
        let mut builder = client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        let response = builder.send().map_err(|e| fail("request failed", &e))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let body = response.bytes().map_err(|e| fail("cannot read response body", &e))?.to_vec();
        Ok(HttpResponse { status, headers, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// One-shot server: records the raw request head and body, answers 201.
    fn serve_once() -> (String, mpsc::Receiver<(String, Vec<u8>)>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            let mut head = String::new();
            let mut len = 0;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head.push_str(&h);
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            write!(w, "HTTP/1.1 201 Created\r\nX-Served: yes\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
            tx.send((head, body)).unwrap();
        });
        (format!("http://{addr}"), rx)
    }

    #[test]
    fn sends_request_and_reads_any_status() {
        let (base, seen) = serve_once();
        let request = HttpRequest {
            method: "PUT".into(),
            url: format!("{base}/v1/pets/7?q=1"),
            headers: vec![("X-Trace".into(), "t-1".into())],
            body: Some(b"{\"a\":1}".to_vec()),
            timeout: Some(Duration::from_secs(5)),
        };
        let response = ReqwestHttp::default().send(&request).unwrap();
        let (head, body) = seen.recv().unwrap();
        assert!(head.starts_with("PUT /v1/pets/7?q=1 HTTP/1.1\r\n"), "{head}");
        assert!(head.to_ascii_lowercase().contains("x-trace: t-1\r\n"), "{head}");
        assert_eq!(body, b"{\"a\":1}");
        assert_eq!(response.status, 201);
        assert_eq!(super::super::header(&response.headers, "x-served"), Some("yes"));
        assert_eq!(response.body, b"ok");
    }

    #[test]
    fn no_response_is_a_transport_error_naming_the_url() {
        // The port was taken and released at once, so the connection will be refused.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/x");
        let request = HttpRequest { method: "GET".into(), url: url.clone(), ..Default::default() };
        let error = ReqwestHttp::default().send(&request).unwrap_err();
        assert!(error.0.contains(&url), "{error}");
    }
}
