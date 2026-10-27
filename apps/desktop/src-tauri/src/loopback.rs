//! Receives the one-time login code on `http://127.0.0.1:<port>/callback`
//! (RFC 8252 loopback redirect). The code alone is useless without the PKCE
//! verifier held by this process.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_REQUEST_BYTES: usize = 8 * 1024;

enum Outcome {
    Code(String),
    Error(String),
    /// Some other request (e.g. `/favicon.ico`); keep waiting.
    Ignored,
}

fn page(title: &str, message: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{font:16px -apple-system,system-ui,sans-serif;color:#37352f;display:grid;\
         place-items:center;height:90vh;margin:0}}h1{{font-size:22px}}</style></head>\
         <body><div><h1>{title}</h1><p>{message}</p></div></body></html>"
    )
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn handle(stream: &mut TcpStream) -> Outcome {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_REQUEST_BYTES {
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }
    let request = String::from_utf8_lossy(&buf);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_default();
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        respond(stream, "400 Bad Request", &page("Bad request", "")).await;
        return Outcome::Ignored;
    };
    if url.path() != "/callback" {
        respond(stream, "404 Not Found", &page("Not found", "")).await;
        return Outcome::Ignored;
    }
    let param = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    if let Some(code) = param("code") {
        respond(
            stream,
            "200 OK",
            &page("Signed in to nanace", "You can close this tab and return to the app."),
        )
        .await;
        return Outcome::Code(code);
    }
    // Error codes are short identifiers; anything else is not echoed.
    let error = param("error")
        .filter(|e| e.len() <= 64 && e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        .unwrap_or_else(|| "unknown_error".to_owned());
    respond(
        stream,
        "200 OK",
        &page(
            "Sign-in failed",
            &format!("Reason: {error}. Return to the app to try again."),
        ),
    )
    .await;
    Outcome::Error(error)
}

/// Waits for the browser to deliver the login code, up to `timeout`.
pub async fn receive_code(listener: TcpListener, timeout: Duration) -> Result<String, String> {
    let wait = async {
        loop {
            let (mut stream, _) = listener
                .accept()
                .await
                .map_err(|err| format!("sign-in listener failed: {err}"))?;
            match handle(&mut stream).await {
                Outcome::Code(code) => return Ok(code),
                Outcome::Error(error) => return Err(describe(&error)),
                Outcome::Ignored => {}
            }
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .map_err(|_| "Sign-in timed out. Try again.".to_owned())?
}

fn describe(error: &str) -> String {
    match error {
        "access_denied" => "Sign-in was cancelled.".to_owned(),
        "domain_not_allowed" => "This Google account's domain is not allowed on this server.".to_owned(),
        "email_not_verified" => "This Google account's email is not verified.".to_owned(),
        "account_conflict" => "This email is already linked to a different Google account.".to_owned(),
        "invalid_state" => "The sign-in session expired. Try again.".to_owned(),
        other => format!("Sign-in failed ({other})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .expect("write");
        let mut out = String::new();
        stream.read_to_string(&mut out).await.expect("read");
        out
    }

    #[tokio::test]
    async fn returns_code_after_ignoring_other_requests() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let task = tokio::spawn(receive_code(listener, Duration::from_secs(5)));
        assert!(get(port, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
        let page = get(port, "/callback?code=abc%2Bdef").await;
        assert!(page.starts_with("HTTP/1.1 200") && !page.contains("abc"));
        assert_eq!(task.await.expect("join"), Ok("abc+def".to_owned()));
    }

    #[tokio::test]
    async fn reports_errors_without_echoing_untrusted_text() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let task = tokio::spawn(receive_code(listener, Duration::from_secs(5)));
        let page = get(port, "/callback?error=%3Cscript%3E").await;
        assert!(!page.contains("<script>") && page.contains("unknown_error"));
        assert_eq!(
            task.await.expect("join"),
            Err("Sign-in failed (unknown_error).".to_owned())
        );
    }
}
