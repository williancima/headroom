//! One-shot loopback callback server for OAuth sign-in.
//!
//! Which outcomes end the wait: a valid callback, an `error` param (state already checked) and a
//! missing/empty `code`. Wrong method (405), wrong path (404), wrong state (400) and empty, cut-off or
//! over-16-KiB requests (400) are answered and the server keeps listening.

use super::OAuthError;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const HEADER_LIMIT: usize = 16 * 1024;

pub struct LoopbackServer {
    listener: TcpListener,
    state: String,
    path: String,
    port: u16,
}

enum Outcome {
    Stray,
    Done(Result<String, OAuthError>),
}

impl LoopbackServer {
    /// Binds 127.0.0.1; port 0 picks a free one. The redirect URI names `localhost`.
    pub async fn bind(port: u16, path: &str, state: String) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, state, path: path.to_string(), port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://localhost:<port><path>`.
    pub fn redirect_uri(&self) -> String {
        self.redirect_uri_for("localhost")
    }

    /// Some providers accept only one spelling of the loopback host (e.g. Devin wants `127.0.0.1`).
    pub fn redirect_uri_for(&self, host: &str) -> String {
        format!("http://{host}:{}{}", self.port, self.path)
    }

    /// Waits for the authorization code. Wrap in `tokio::time::timeout` to bound it.
    /// Each connection is read in its own task so a browser preconnect cannot block the real callback.
    pub async fn wait(self) -> Result<String, OAuthError> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (state, path) = (self.state, self.path);
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let (tx, state, path) = (tx.clone(), state.clone(), path.clone());
                    tokio::spawn(async move {
                        if let Outcome::Done(r) = serve(stream, &state, &path).await {
                            let _ = tx.send(r);
                        }
                    });
                }
                Some(result) = rx.recv() => return result,
            }
        }
    }
}

async fn serve(mut stream: TcpStream, state: &str, path: &str) -> Outcome {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let take = (HEADER_LIMIT - buf.len()).min(chunk.len());
        let n = if take == 0 { 0 } else { stream.read(&mut chunk[..take]).await.unwrap_or(0) };
        // Junk never ends the wait: browsers open and drop speculative preconnects.
        if n == 0 {
            if !buf.is_empty() {
                reply(&mut stream, 400, "The sign-in request was incomplete.").await;
            }
            return Outcome::Stray;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let (status, body, outcome) = handle(&head, state, path);
    reply(&mut stream, status, body).await;
    outcome
}

fn handle(head: &str, state: &str, path: &str) -> (u16, &'static str, Outcome) {
    let line = head.split("\r\n").next().unwrap_or("");
    let target = line.split(' ').nth(1);
    let Some(target) = target.filter(|_| line.starts_with("GET ")) else {
        return (405, "Only GET is supported.", Outcome::Stray);
    };
    let Ok(url) = reqwest::Url::parse(&format!("http://localhost{target}")) else {
        return (404, "Sign-in callback not found.", Outcome::Stray);
    };
    if url.path() != path {
        return (404, "Sign-in callback not found.", Outcome::Stray);
    }
    let value = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
    if value("state").as_deref() != Some(state) {
        return (400, "Sign-in state did not match.", Outcome::Stray);
    }
    if let Some(error) = value("error") {
        return (400, "The provider did not authorize the sign-in. Try again from Headroom.", Outcome::Done(Err(OAuthError::AuthorizationFailed(error))));
    }
    match value("code").filter(|c| !c.is_empty()) {
        Some(code) => (200, "Your account is connected.", Outcome::Done(Ok(code))),
        None => (400, "Authorization code was missing.", Outcome::Done(Err(OAuthError::InvalidCallback))),
    }
}

const PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Headroom</title>
<style>
:root{color-scheme:light dark;--bg:#f4f5f7;--card:#fff;--fg:#1c1e21;--muted:#62676e;--line:#e1e4e8;--accent:#2f9e57}
@media (prefers-color-scheme:dark){:root{--bg:#16181b;--card:#202327;--fg:#eceef0;--muted:#9aa0a8;--line:#30343a;--accent:#4cc77a}}
.error{--accent:#d64545}
@media (prefers-color-scheme:dark){.error{--accent:#ef6b6b}}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;padding:24px;background:var(--bg);color:var(--fg);font:16px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif}
main{width:100%;max-width:380px;padding:32px 28px;text-align:center;background:var(--card);border:1px solid var(--line);border-radius:16px;box-shadow:0 8px 30px rgba(0,0,0,.08)}
.brand{margin:0 0 20px;font-size:13px;font-weight:700;letter-spacing:.14em;text-transform:uppercase;color:var(--muted)}
svg{width:56px;height:56px;color:var(--accent)}
h1{margin:16px 0 8px;font-size:22px}
p{margin:0 0 6px}
.hint{font-size:14px;color:var(--muted)}
</style></head>
<body class="{class}"><main>
<p class="brand">Headroom</p>
{icon}
<h1>{heading}</h1>
<p>{message}</p>
<p class="hint">You can close this tab and return to Headroom.</p>
</main></body></html>"##;

const ICON_OK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" role="img" aria-label="Success"><circle cx="12" cy="12" r="10"/><path d="m7.5 12.5 3 3 6-7"/></svg>"#;
const ICON_ERR: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" role="img" aria-label="Error"><circle cx="12" cy="12" r="10"/><path d="M12 7v6M12 16.5v.01"/></svg>"#;

/// `message` must be static text: it is inserted unescaped and never echoes request data.
fn page(status: u16, message: &str) -> String {
    let (class, icon, heading) =
        if status == 200 { ("ok", ICON_OK, "Signed In") } else { ("error", ICON_ERR, "Sign-In Failed") };
    PAGE.replace("{class}", class).replace("{icon}", icon).replace("{heading}", heading).replace("{message}", message)
}

async fn reply(stream: &mut TcpStream, status: u16, body: &str) {
    let html = page(status, body);
    let reason = if status == 200 { "OK" } else { "Error" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends `request` raw and returns the reply (read to EOF).
    async fn send(port: u16, request: &str, close_write: bool) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        if close_write {
            s.shutdown().await.unwrap();
        }
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        out
    }

    fn get(query: &str) -> String {
        format!("GET /callback{query} HTTP/1.1\r\nHost: localhost\r\n\r\n")
    }

    async fn server() -> (u16, tokio::task::JoinHandle<Result<String, OAuthError>>) {
        let s = LoopbackServer::bind(0, "/callback", "expected".into()).await.unwrap();
        assert!(s.redirect_uri().starts_with("http://localhost:"));
        assert!(s.redirect_uri().ends_with("/callback"));
        assert_eq!(s.redirect_uri_for("127.0.0.1"), format!("http://127.0.0.1:{}/callback", s.port()));
        let port = s.port();
        (port, tokio::spawn(s.wait()))
    }

    #[tokio::test]
    async fn strays_are_answered_and_the_wait_continues_until_the_code() {
        let (port, task) = server().await;
        assert!(send(port, &get("?code=x&state=wrong"), false).await.starts_with("HTTP/1.1 400 "));
        assert!(send(port, "GET /cancel?code=x&state=expected HTTP/1.1\r\n\r\n", false).await.starts_with("HTTP/1.1 404 "));
        assert!(send(port, "POST /callback?code=x&state=expected HTTP/1.1\r\n\r\n", false).await.starts_with("HTTP/1.1 405 "));
        assert!(!task.is_finished());
        let ok = send(port, &get("?code=a%20b&state=expected"), false).await;
        assert!(ok.starts_with("HTTP/1.1 200 ") && ok.contains("<h1>Signed In</h1>"), "{ok}");
        assert_eq!(task.await.unwrap().unwrap(), "a b");
    }

    #[test]
    fn pages_show_the_outcome() {
        let ok = page(200, "Your account is connected.");
        assert!(ok.contains("<h1>Signed In</h1>") && ok.contains("Your account is connected.") && !ok.contains("Sign-In Failed"));
        let err = page(400, "The provider did not authorize the sign-in. Try again from Headroom.");
        assert!(err.contains("<h1>Sign-In Failed</h1>") && err.contains("The provider did not authorize the sign-in. Try again from Headroom.") && !err.contains("Signed In"));
        for p in [&ok, &err] {
            for ph in ["{class}", "{icon}", "{heading}", "{message}"] {
                assert!(!p.contains(ph));
            }
        }
    }

    #[tokio::test]
    async fn error_page_is_served_with_a_correct_length() {
        let (port, task) = server().await;
        let r = send(port, &get("?error=access_denied&state=expected"), false).await;
        let (head, body) = r.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 400 ") && body.contains("<h1>Sign-In Failed</h1>"), "{r}");
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
        let _ = task.await;
    }

    #[tokio::test]
    async fn error_param_fails_after_the_state_check() {
        let (port, task) = server().await;
        // Wrong state with an error does not end the wait.
        send(port, &get("?error=denied&state=wrong"), false).await;
        assert!(!task.is_finished());
        assert!(send(port, &get("?error=access_denied&state=expected"), false).await.starts_with("HTTP/1.1 400 "));
        assert!(matches!(task.await.unwrap(), Err(OAuthError::AuthorizationFailed(e)) if e == "access_denied"));
    }

    #[tokio::test]
    async fn missing_or_empty_code_is_an_invalid_callback() {
        for q in ["?state=expected", "?code=&state=expected"] {
            let (port, task) = server().await;
            send(port, &get(q), false).await;
            assert!(matches!(task.await.unwrap(), Err(OAuthError::InvalidCallback)));
        }
    }

    #[tokio::test]
    async fn empty_cut_off_or_oversized_requests_are_refused_and_the_wait_continues() {
        let (port, task) = server().await;
        assert_eq!(send(port, "", true).await, "");
        let reply = send(port, "GET /callback?code=x&state=expected HTTP/1.1\r\nHost: l\r\n", true).await;
        assert!(reply.starts_with("HTTP/1.1 400 "), "{reply}");
        let big = format!("GET /callback?code=x&state=expected&pad={} HTTP/1.1\r\n\r\n", "a".repeat(20_000));
        assert!(send(port, &big, false).await.starts_with("HTTP/1.1 400 "));
        assert!(!task.is_finished());
        send(port, &get("?code=ok&state=expected"), false).await;
        assert_eq!(task.await.unwrap().unwrap(), "ok");
    }

    #[tokio::test]
    async fn header_split_across_writes_is_reassembled() {
        let (port, task) = server().await;
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"GET /callback?code=demo&sta").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!task.is_finished());
        s.write_all(b"te=expected HTTP/1.1\r\n\r\n").await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(), "demo");
    }

    #[tokio::test]
    async fn bind_failure_is_an_io_error() {
        let a = LoopbackServer::bind(0, "/callback", "s".into()).await.unwrap();
        let port = a.port();
        assert!(LoopbackServer::bind(port, "/callback", "s".into()).await.is_err());
    }
}
