//! `langspan dashboard`: a read-only page of what the coordinator knows, for a
//! browser on this machine. One self-contained page, and one JSON view
//! fetched through the same socket client as every other interface.
//!
//! The listener is loopback-only unless the owner says otherwise. Every
//! request needs the per-run token; the `Host` header must name the address
//! the connection arrived at, which defeats DNS rebinding; only small `GET`
//! requests without a body are read, each within a deadline; and at most
//! `MAX_CONNECTIONS` are served at once. See ADR 0013.

use crate::{
    client,
    protocol::{PROTOCOL_VERSION, Request},
};
use langspan_core::{Command, Outcome};
use std::{
    fs,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub const DEFAULT_PORT: u16 = 7311;
/// Connections served at once; more are closed unanswered.
pub const MAX_CONNECTIONS: usize = 16;
/// The request line and headers together, at most.
pub const MAX_HEAD_BYTES: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// A whole request must arrive within this, however slowly it trickles.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
/// The header a script sends the token in.
pub const TOKEN_HEADER: &str = "x-langspan-token";
/// The file `--open` hands the browser, so the token never appears in a
/// process's arguments, which other users of the machine can list.
const OPEN_FILE: &str = "dashboard-open.html";

const PAGE: &str = include_str!("dashboard.html");

pub struct Options {
    pub listen: SocketAddr,
    pub allow_network: bool,
    pub open: bool,
}

pub fn default_listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)
}

/// Refuse a non-loopback address unless the owner allowed it. Returns
/// whether a warning is due.
pub fn check_listen(listen: SocketAddr, allow_network: bool) -> Result<bool, crate::Error> {
    if listen.ip().is_loopback() {
        return Ok(false);
    }
    if !allow_network {
        return Err(format!(
            "refusing to listen on {listen}: it is not a loopback address. The dashboard shows \
             every session's task lines, files and messages. To reach it from another device, \
             forward the port over SSH (ssh -L {DEFAULT_PORT}:127.0.0.1:{DEFAULT_PORT} HOST) or \
             use a VPN to this machine; add --allow-network only if you accept that anyone who \
             can reach {listen} and learns the token can read it"
        )
        .into());
    }
    Ok(true)
}

struct Context {
    state_dir: PathBuf,
    token: String,
    csp: String,
}

/// Bind, print the URL, and serve until the process is stopped.
pub fn serve(state_dir: PathBuf, options: Options) -> Result<(), crate::Error> {
    let warn = check_listen(options.listen, options.allow_network)?;
    let token = random_token()?;
    let csp = content_security_policy(PAGE)?;
    let listener = TcpListener::bind(options.listen)
        .map_err(|error| format!("cannot listen on {}: {error}", options.listen))?;
    let bound = listener.local_addr()?;
    if warn {
        eprintln!(
            "{}",
            serde_json::json!({"level":"warn","event":"dashboard_on_network","address":bound.to_string(),
                "detail":"the dashboard is reachable beyond this machine; anyone with the token can read every session's work"})
        );
    }
    // An unspecified bind is reached locally through loopback.
    let visible = match bound.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bound.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), bound.port())
        }
        _ => bound,
    };
    let url = format!("http://{visible}/?t={token}");
    println!("{url}");
    io::stdout().flush()?;
    eprintln!(
        "{}",
        serde_json::json!({"level":"info","event":"dashboard_ready","address":bound.to_string()})
    );
    if options.open
        && let Err(error) = open_browser(&state_dir, &url)
    {
        eprintln!(
            "{}",
            serde_json::json!({"level":"warn","event":"dashboard_open_failed","detail":error.to_string()})
        );
    }
    let context = Arc::new(Context {
        state_dir,
        token,
        csp,
    });
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"warn","event":"dashboard_accept_failed","detail":error.to_string()})
                );
                // A persistent failure such as running out of descriptors
                // must not spin.
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            drop(stream);
            continue;
        }
        let context = context.clone();
        let slot = Slot(active.clone());
        let spawned = std::thread::Builder::new()
            .name("langspan-dashboard".into())
            .spawn(move || {
                let _slot = slot;
                let _ = connection(stream, &context); // The client went away.
            });
        if let Err(error) = spawned {
            eprintln!(
                "{}",
                serde_json::json!({"level":"warn","event":"dashboard_thread_failed","detail":error.to_string()})
            );
        }
    }
    Ok(())
}

/// Releases a connection slot however its thread ends.
struct Slot(Arc<AtomicUsize>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    allow: bool,
}

impl Reply {
    fn text(status: u16, message: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: format!("{message}\n").into_bytes(),
            allow: false,
        }
    }
}

fn connection(mut stream: TcpStream, context: &Context) -> io::Result<()> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let arrived = stream.local_addr()?;
    let reply = match read_head(&mut stream) {
        Ok(head) => match parse_head(&head) {
            Ok(request) => route(&request, arrived, context),
            Err(reply) => reply,
        },
        Err(reply) => reply,
    };
    write_reply(&mut stream, &reply, &context.csp)?;
    // Closing with unread input would reset the connection and could
    // discard the reply before the client reads it: read what is left, within
    // bounds, first.
    stream.shutdown(std::net::Shutdown::Write)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut drained = 0;
    let mut chunk = [0u8; 4096];
    while drained < 64 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => drained += read,
        }
    }
    Ok(())
}

/// The request line and headers, bounded in size and in total time.
fn read_head(stream: &mut TcpStream) -> Result<Vec<u8>, Reply> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| Reply::text(408, "request not received in time"))?;
        stream
            .set_read_timeout(Some(left.min(IO_TIMEOUT)))
            .map_err(|_| Reply::text(500, "cannot set a read deadline"))?;
        let read = match stream.read(&mut chunk) {
            Ok(0) => return Err(Reply::text(400, "incomplete request")),
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(Reply::text(408, "request not received in time"));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Reply::text(400, "unreadable request")),
        };
        head.extend_from_slice(&chunk[..read]);
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            if end + 4 > MAX_HEAD_BYTES {
                return Err(Reply::text(431, "request header too large"));
            }
            // Anything after the head is a body, refused by its headers, or
            // a pipelined request, which is not served.
            head.truncate(end);
            return Ok(head);
        }
        if head.len() > MAX_HEAD_BYTES {
            return Err(Reply::text(431, "request header too large"));
        }
    }
}

struct Parsed {
    method: String,
    path: String,
    query: String,
    /// Lower-case names.
    headers: Vec<(String, String)>,
}

impl Parsed {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn parse_head(head: &[u8]) -> Result<Parsed, Reply> {
    let bad = || Reply::text(400, "malformed request");
    let text = std::str::from_utf8(head).map_err(|_| bad())?;
    let mut lines = text.split("\r\n");
    let line = lines.next().ok_or_else(bad)?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") || !target.starts_with('/') {
        return Err(bad());
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(bad)?;
        if name.is_empty() || name.bytes().any(|byte| byte <= b' ' || byte >= 0x7f) {
            return Err(bad());
        }
        let name = name.to_ascii_lowercase();
        if name == "host" && headers.iter().any(|(key, _)| key == "host") {
            return Err(bad());
        }
        headers.push((name, value.trim().to_owned()));
        if headers.len() > MAX_HEADERS {
            return Err(Reply::text(431, "too many request headers"));
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Parsed {
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers,
    })
}

/// The `Host` values a browser sends for the address it connected to.
fn host_allowed(host: &str, arrived: SocketAddr) -> bool {
    let port = arrived.port();
    host == arrived.to_string()
        || (arrived.ip().is_loopback() && host == format!("localhost:{port}"))
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |difference, (x, y)| difference | (x ^ y))
            == 0
}

fn route(request: &Parsed, arrived: SocketAddr, context: &Context) -> Reply {
    if request.method != "GET" {
        return Reply {
            allow: true,
            ..Reply::text(405, "only GET is served")
        };
    }
    let has_body = request
        .header("content-length")
        .is_some_and(|length| length != "0")
        || request.header("transfer-encoding").is_some();
    if has_body {
        return Reply::text(413, "a request body is not accepted");
    }
    if !request
        .header("host")
        .is_some_and(|host| host_allowed(host, arrived))
    {
        return Reply::text(
            421,
            "the Host header does not name this dashboard's address; open the URL langspan dashboard printed",
        );
    }
    if request.path != "/" && request.path != "/api/state" {
        return Reply::text(404, "not found");
    }
    let supplied = request
        .query
        .split('&')
        .find_map(|pair| pair.strip_prefix("t="))
        .or_else(|| request.header(TOKEN_HEADER));
    match supplied {
        None => {
            return Reply::text(
                401,
                "this dashboard needs its access token: open the URL langspan dashboard printed",
            );
        }
        Some(token) if !same(token, &context.token) => {
            return Reply::text(
                403,
                "wrong access token: open the URL this run of langspan dashboard printed",
            );
        }
        Some(_) => {}
    }
    if request.path == "/" {
        return Reply {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: PAGE.as_bytes().to_vec(),
            allow: false,
        };
    }
    state(&context.state_dir)
}

/// The coordinator's view through the ordinary socket client. Markup
/// characters are escaped even inside JSON strings, so the body cannot be
/// read as HTML whatever a client does with it.
fn state(state_dir: &Path) -> Reply {
    let request = Request {
        version: PROTOCOL_VERSION,
        request_id: "read".into(),
        command: Command::Dashboard,
    };
    let (status, body) = match client::call(state_dir, &request) {
        Ok(response) => match response.outcome {
            Outcome::Dashboard { dashboard } => {
                // The install command for each uncovered host, naming this
                // coordinator's state directory (roadmap A7).
                let advice: serde_json::Map<String, serde_json::Value> = dashboard
                    .uninstalled_hosts
                    .iter()
                    .map(|host| {
                        (
                            host.clone(),
                            crate::hosts::preview_command(host, state_dir).into(),
                        )
                    })
                    .collect();
                (
                    200,
                    serde_json::json!({"available": true, "dashboard": dashboard, "install_advice": advice}),
                )
            }
            Outcome::Error { code, message } => (
                503,
                serde_json::json!({"available": false, "error": format!("{code}: {message}")}),
            ),
            _ => (
                503,
                serde_json::json!({"available": false, "error": "the coordinator answered with something other than a dashboard view; restart the daemon after upgrading LangSpan"}),
            ),
        },
        Err(error) => (
            503,
            serde_json::json!({"available": false, "error": error.to_string()}),
        ),
    };
    let json = body
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    Reply {
        status,
        content_type: "application/json; charset=utf-8",
        body: json.into_bytes(),
        allow: false,
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        421 => "Misdirected Request",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

fn write_reply(stream: &mut TcpStream, reply: &Reply, csp: &str) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         Content-Security-Policy: {csp}\r\nX-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n\
         Cross-Origin-Resource-Policy: same-origin\r\nConnection: close\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type,
        reply.body.len()
    );
    if reply.allow {
        head.push_str("Allow: GET\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

/// Allow exactly the page's own script and style, by hash, and nothing from
/// anywhere else.
fn content_security_policy(page: &str) -> Result<String, crate::Error> {
    let inline = |open: &str, close: &str| -> Result<String, crate::Error> {
        let start = page
            .find(open)
            .ok_or("the dashboard page has no inline block")?
            + open.len();
        let end = start
            + page[start..]
                .find(close)
                .ok_or("the dashboard page has an unclosed inline block")?;
        if page[end..].contains(open) {
            return Err("the dashboard page must have one script and one style block".into());
        }
        Ok(format!(
            "'sha256-{}'",
            base64(&crate::sha256::digest(&page.as_bytes()[start..end]))
        ))
    };
    Ok(format!(
        "default-src 'none'; script-src {}; style-src {}; connect-src 'self'; img-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        inline("<script>", "</script>")?,
        inline("<style>", "</style>")?
    ))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..4 {
            if index <= chunk.len() {
                text.push(char::from(
                    ALPHABET[((n >> (18 - 6 * index)) & 63) as usize],
                ));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// 256 bits from the operating system, as hex. No randomness, no dashboard.
fn random_token() -> Result<String, crate::Error> {
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| format!("cannot read randomness for the access token: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Hand the browser an owner-only file that forwards to the URL, then
/// remove the file once the browser has had time to read it.
fn open_browser(state_dir: &Path, url: &str) -> Result<(), crate::Error> {
    let directory = crate::local::prepare_directory(state_dir)?;
    let path = directory.join(OPEN_FILE);
    let mut file = crate::local::private_file(&path, true)?;
    file.set_len(0)?;
    file.write_all(
        format!(
            "<!doctype html><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\">\
             <meta http-equiv=\"refresh\" content=\"0; url={url}\"><title>LangSpan dashboard</title>\
             <a href=\"{url}\">Open the LangSpan dashboard</a>\n"
        )
        .as_bytes(),
    )?;
    drop(file);
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(opener)
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run {opener}: {error}; open the printed URL yourself"))?;
    std::thread::Builder::new()
        .name("langspan-dashboard-open".into())
        .spawn(move || {
            let _ = child.wait();
            std::thread::sleep(Duration::from_secs(30));
            let _ = fs::remove_file(path);
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output);
        }
    }

    #[test]
    fn the_policy_names_the_page_blocks_by_hash_and_nothing_inline_otherwise() {
        let csp = content_security_policy(PAGE).unwrap();
        assert!(!csp.contains("unsafe"), "{csp}");
        assert!(csp.contains("default-src 'none'"), "{csp}");
        assert_eq!(csp.matches("'sha256-").count(), 2, "{csp}");
    }

    #[test]
    fn only_the_arrival_address_is_an_acceptable_host() {
        let loopback: SocketAddr = "127.0.0.1:7311".parse().unwrap();
        assert!(host_allowed("127.0.0.1:7311", loopback));
        assert!(host_allowed("localhost:7311", loopback));
        assert!(!host_allowed("localhost", loopback));
        assert!(!host_allowed("evil.example:7311", loopback));
        assert!(!host_allowed("127.0.0.1:7312", loopback));
        let v6: SocketAddr = "[::1]:7311".parse().unwrap();
        assert!(host_allowed("[::1]:7311", v6));
        let lan: SocketAddr = "192.168.1.5:7311".parse().unwrap();
        assert!(host_allowed("192.168.1.5:7311", lan));
        assert!(!host_allowed("localhost:7311", lan));
    }

    #[test]
    fn non_loopback_addresses_need_the_owners_say_so() {
        assert!(!check_listen("127.0.0.1:1".parse().unwrap(), false).unwrap());
        assert!(!check_listen("[::1]:1".parse().unwrap(), false).unwrap());
        assert!(check_listen("0.0.0.0:1".parse().unwrap(), false).is_err());
        assert!(check_listen("[::ffff:127.0.0.1]:1".parse().unwrap(), false).is_err());
        assert!(check_listen("0.0.0.0:1".parse().unwrap(), true).unwrap());
    }
}
