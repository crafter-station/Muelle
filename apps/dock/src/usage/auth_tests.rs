//! Synthetic credentials and loopback HTTP only; never read a user's session.
use super::*;
use std::{
    io::Write,
    net::{TcpListener, TcpStream},
    sync::atomic::AtomicUsize,
};

struct Credentials(PathBuf);
impl Credentials {
    fn new(value: &Value) -> anyhow::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = env::temp_dir().join(format!(
            "muelle-usage-auth-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, serde_json::to_vec(value)?)?;
        Ok(Self(path))
    }
}
impl Drop for Credentials {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Server {
    endpoint: String,
    thread: Option<thread::JoinHandle<anyhow::Result<Vec<String>>>>,
}
impl Server {
    fn new(replies: Vec<(u16, Value)>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let thread = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, value) in replies {
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return Ok(requests);
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                requests.push(read_request(&mut stream)?);
                let body = serde_json::to_vec(&value)?;
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )?;
                stream.write_all(&body)?;
            }
            Ok(requests)
        });
        Ok(Self {
            endpoint,
            thread: Some(thread),
        })
    }

    fn requests(mut self) -> anyhow::Result<Vec<String>> {
        self.thread
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing test server"))?
            .join()
            .map_err(|_| anyhow::anyhow!("test server panicked"))?
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> anyhow::Result<String> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    while bytes.len() < 16384 {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end])?;
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("Content-Length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    Ok(String::from_utf8(bytes)?)
}

fn client() -> anyhow::Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?)
}

#[test]
fn claude_forbidden_quota_is_not_a_logged_out_session() -> anyhow::Result<()> {
    let auth = Credentials::new(&serde_json::json!({"claudeAiOauth": {"accessToken": "fixture"}}))?;
    let server = Server::new(vec![(
        403,
        serde_json::json!({"error": {"type": "permission_error"}}),
    )])?;
    let usage = fetch_claude_at(&client()?, &auth.0, &server.endpoint);
    assert!(usage.state == State::Unavailable);
    assert_eq!(server.requests()?.len(), 1);
    Ok(())
}

#[test]
fn gemini_does_not_discard_a_token_with_thirty_seconds_remaining() -> anyhow::Result<()> {
    let auth = Credentials::new(&serde_json::json!({
        "access_token": "fixture", "expiry_date": Utc::now().timestamp_millis() + 30_000
    }))?;
    let before = fs::read(&auth.0)?;
    let server = Server::new(vec![
        (
            200,
            serde_json::json!({"cloudaicompanionProject": "fixture-project"}),
        ),
        (
            200,
            serde_json::json!({"buckets": [{"modelId":"gemini-3-pro", "remainingFraction":0.6}]}),
        ),
    ])?;
    let usage = fetch_gemini_cli_at(
        &client()?,
        &auth.0,
        &format!("{}/v1internal", server.endpoint),
    );
    assert!(usage.state == State::Ready);
    assert_eq!(usage.compact_text(), "40%");
    let requests = server.requests()?;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contains("\"project\":\"fixture-project\""));
    assert_eq!(
        fs::read(&auth.0)?,
        before,
        "Muelle must not rewrite credentials"
    );
    Ok(())
}

#[test]
fn custom_credential_directories_follow_each_clients_own_convention() {
    let home = Path::new(r"C:\fixture\home");
    let custom = Path::new(r"C:\fixture\custom");
    assert_eq!(
        auth::claude_path(None, Some(home)),
        Some(home.join(".claude/.credentials.json"))
    );
    assert_eq!(
        auth::claude_path(Some(custom), Some(home)),
        Some(custom.join(".credentials.json"))
    );
    assert_eq!(
        auth::gemini_path(None, Some(home)),
        Some(home.join(".gemini/oauth_creds.json"))
    );
    assert_eq!(
        auth::gemini_path(Some(custom), Some(home)),
        Some(custom.join(".gemini/oauth_creds.json"))
    );
    assert_eq!(
        auth::claude_path(Some(Path::new("")), Some(home)),
        auth::claude_path(None, Some(home))
    );
    assert_eq!(
        auth::gemini_path(Some(Path::new("")), Some(home)),
        auth::gemini_path(None, Some(home))
    );
    // An unusable explicit override must not select another stored account.
    assert!(auth::claude_path(Some(Path::new("relative")), Some(home)).is_none());
    assert!(auth::gemini_path(Some(Path::new("relative")), Some(home)).is_none());
}

#[test]
fn credentials_distinguish_absent_from_unreadable_invalid_or_oversized() -> anyhow::Result<()> {
    let file = Credentials::new(&serde_json::json!({}))?;
    fs::write(&file.0, b"{")?;
    assert!(matches!(
        auth::read_credentials(&file.0, Provider::Claude),
        Err(State::Unavailable)
    ));
    fs::write(&file.0, vec![b' '; 1024 * 1024 + 1])?;
    assert!(matches!(
        auth::read_credentials(&file.0, Provider::Gemini),
        Err(State::Unavailable)
    ));
    // Model a client replacing credentials while Muelle polls, without ACL
    // changes or touching a real account's files.
    use std::os::windows::fs::OpenOptionsExt;
    fs::write(&file.0, b"{}")?;
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&file.0)?;
    assert!(matches!(
        auth::read_credentials(&file.0, Provider::Claude),
        Err(State::Unavailable)
    ));
    drop(locked);
    assert!(auth::read_credentials(&file.0, Provider::Claude).is_ok());
    fs::remove_file(&file.0)?;
    assert!(matches!(
        auth::read_credentials(&file.0, Provider::Claude),
        Err(State::SignIn)
    ));
    Ok(())
}

#[test]
fn gemini_keeps_one_quota_source_and_does_not_reuse_the_other_clients_cache() {
    let mut monitor = Monitor::new(0, 0);
    let mut clients = ActiveClients::default();
    clients.providers.0[Provider::Gemini as usize] = true;
    monitor.record_active(clients);
    let measured = ready(vec![Window {
        label: Some("Pro".into()),
        minutes: 0,
        used_percent: 40,
        resets_at: None,
    }]);
    monitor
        .current
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .providers[Provider::Gemini as usize] = measured.clone();
    monitor.record_active(clients);
    assert_eq!(
        monitor.snapshot().get(Provider::Gemini).compact_text(),
        "40%"
    );
    clients.antigravity = true;
    monitor.record_active(clients);
    assert!(monitor.snapshot().get(Provider::Gemini).windows.is_empty());
    monitor
        .current
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .providers[Provider::Gemini as usize] = measured;
    monitor.mark_inactive();
    clients.antigravity = false;
    monitor.record_active(clients);
    assert!(monitor.snapshot().get(Provider::Gemini).windows.is_empty());
}

#[test]
fn gemini_stops_after_a_rejected_account_lookup_and_classifies_auth_errors() -> anyhow::Result<()> {
    let auth = Credentials::new(&serde_json::json!({"access_token": "fixture"}))?;
    for (status, expected) in [
        (401, State::SignIn),
        (403, State::Unavailable),
        (429, State::Unavailable),
        (503, State::Unavailable),
    ] {
        // A second reply catches an unnecessary quota request after a failed lookup.
        let server = Server::new(vec![
            (status, serde_json::json!({})),
            (200, serde_json::json!({})),
        ])?;
        let usage = fetch_gemini_cli_at(
            &client()?,
            &auth.0,
            &format!("{}/v1internal", server.endpoint),
        );
        assert!(usage.state == expected);
        assert_eq!(
            server.requests()?.len(),
            1,
            "a rejected lookup must not trigger another query"
        );
    }
    Ok(())
}
