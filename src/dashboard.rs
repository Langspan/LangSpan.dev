//! `langspan dashboard`: a read-only page of what the coordinator knows, for a
//! browser on this machine. One self-contained page, and one JSON view
//! fetched through the same socket client as every other interface.
//!
//! The listener is loopback-only unless the owner says otherwise. Every
//! request needs the per-run token; the `Host` header must name the address
//! the connection arrived at, which defeats DNS rebinding; only small `GET`
//! requests without a body are read, each within a deadline; and at most
//! `MAX_CONNECTIONS` are served at once. See ADR 0013.

use crate::{client, count, protocol::Request};
use langspan_core::{Command, DashboardEvent, DashboardEvents, Outcome};
use std::{
    fs,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
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

/// The page: one inline style block, one inline script block, and markers
/// that `page` fills.
const SHELL: &str = include_str!("dashboard.html");
/// Fictional scenes for `--demo`, shown labeled as simulated. The live page
/// never contains them.
const DEMO_SCENES: &str = include_str!("dashboard-demo.js");
/// The brand lockup (mark and outlined wordmark), from the approved kit,
/// used as is.
const LOCKUP_LIGHT: &str = include_str!("../docs/assets/brand/langspan-lockup-light.svg");
const LOCKUP_DARK: &str = include_str!("../docs/assets/brand/langspan-lockup-dark.svg");
const MARK_MARKER: &str = "<!--langspan:brand-mark-->";
const DEMO_MARKER: &str = "/*langspan:demo-scenes*/";

pub struct Options {
    pub listen: SocketAddr,
    pub allow_network: bool,
    pub open: bool,
    /// Serve the simulated demonstration instead of the coordinator's view.
    pub demo: bool,
}

/// The kit's artwork as inline markup: every line after the header as the
/// file has it, without the namespace and fixed size a standalone file
/// needs, and hidden from assistive technology because the page names the
/// product in text.
fn brand_mark(svg: &str, class: &str) -> Result<String, crate::Error> {
    let unexpected = || "the brand file does not start with the kit's header";
    let (header, body) = svg.trim().split_once('\n').ok_or_else(unexpected)?;
    let view_box = header
        .strip_prefix("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"")
        .and_then(|rest| rest.split_once('"'))
        .filter(|(_, rest)| rest.ends_with(" fill=\"none\">"))
        .map(|(view_box, _)| view_box)
        .ok_or_else(unexpected)?;
    Ok(format!(
        "<svg class=\"{class}\" viewBox=\"{view_box}\" fill=\"none\" aria-hidden=\"true\" focusable=\"false\">\n{body}"
    ))
}

/// The page served at `/`: the live view, or with `demo` the same page and
/// renderer with the simulated scenes in place of the coordinator.
pub fn page(demo: bool) -> Result<String, crate::Error> {
    if SHELL.matches(MARK_MARKER).count() != 1 || SHELL.matches(DEMO_MARKER).count() != 1 {
        return Err("the dashboard page must have one brand-mark and one demo marker".into());
    }
    let mark = format!(
        "{}{}",
        brand_mark(LOCKUP_LIGHT, "ls-mark ls-mark-light")?,
        brand_mark(LOCKUP_DARK, "ls-mark ls-mark-dark")?
    );
    let scenes = if demo {
        DEMO_SCENES
    } else {
        "const DEMO_SCENES = null;"
    };
    Ok(SHELL
        .replace(MARK_MARKER, &mark)
        .replace(DEMO_MARKER, scenes))
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
    page: String,
    demo: bool,
    /// The local month last counted and its day starts, found once a month
    /// per run.
    month: Mutex<Option<(count::Month, Vec<u64>)>>,
}

/// Bind, print the URL, and serve until the process is stopped.
pub fn serve(state_dir: PathBuf, options: Options) -> Result<(), crate::Error> {
    let warn = check_listen(options.listen, options.allow_network)?;
    let token = random_token()?;
    let page = page(options.demo)?;
    let csp = content_security_policy(&page)?;
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
        serde_json::json!({"level":"info","event":"dashboard_ready","address":bound.to_string(),
            "data": if options.demo { "simulated" } else { "coordinator" }})
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
        page,
        demo: options.demo,
        month: Mutex::new(None),
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
    if !matches!(request.path.as_str(), "/" | "/api/state" | "/api/events") {
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
            body: context.page.as_bytes().to_vec(),
            allow: false,
        };
    }
    if context.demo {
        return Reply::text(
            404,
            "this dashboard serves simulated data only; run langspan dashboard without --demo for the coordinator's view",
        );
    }
    if request.path == "/api/events" {
        return events(context, &request.query);
    }
    state(context)
}

/// The coordinator's view through the ordinary socket client.
fn state(context: &Context) -> Reply {
    let state_dir = context.state_dir.as_path();
    let request = Request::new("read", Command::Dashboard);
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
                            crate::install::detect::preview_command(host, state_dir).into(),
                        )
                    })
                    .collect();
                (
                    200,
                    serde_json::json!({
                        "available": true,
                        "dashboard": dashboard,
                        "install_advice": advice,
                        // This local month's refusals (roadmap H5).
                        "refusals_this_month": month_count(context),
                    }),
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
    json_reply(status, &body)
}

/// Markup characters are escaped even inside JSON strings, so the body
/// cannot be read as HTML whatever a client does with it.
fn json_reply(status: u16, body: &serde_json::Value) -> Reply {
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

/// Rows a page holds unless the query says otherwise.
const FEED_LIMIT: u32 = 100;

/// `after` and `limit` from the query, defaulting to 0 and `FEED_LIMIT`;
/// `limit` within the store's own bound of 1 to 1000.
fn feed_query(query: &str) -> Result<(u64, u32), String> {
    let (mut after, mut limit) = (0, FEED_LIMIT);
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("after=") {
            after = value
                .parse()
                .map_err(|_| format!("after must be a whole number between 0 and {}", u64::MAX))?;
        } else if let Some(value) = pair.strip_prefix("limit=") {
            limit = value
                .parse()
                .ok()
                .filter(|limit| (1..=1000).contains(limit))
                .ok_or("limit must be a whole number between 1 and 1000")?;
        }
    }
    Ok((after, limit))
}

/// One row per recorded event, reduced to an allowlist: its type, the first
/// session, workspace and time it names (breadth first, so a field of the
/// event wins over one nested in it), every `path` and `paths` it carries,
/// and the typed story facts of `DashboardEvent::detail`. Nothing else, so
/// prompt summaries, message and note text, task lines, payloads and any
/// field added later are never served.
fn feed_page(after: u64, events: &[langspan_core::RecordedEvent]) -> DashboardEvents {
    use serde_json::Value;
    let rows = events.iter().map(|recorded| {
        let value = serde_json::to_value(&recorded.event).unwrap_or_default();
        let mut queue = std::collections::VecDeque::from([&value]);
        let (mut session_id, mut workspace_id, mut at_ms) = (None, None, None);
        let mut paths = Vec::new();
        while let Some(next) = queue.pop_front() {
            match next {
                Value::Object(fields) => {
                    for (key, field) in fields {
                        match (key.as_str(), field) {
                            ("session_id", Value::String(id)) => {
                                session_id = session_id.or(Some(id.as_str()));
                            }
                            ("workspace_id", Value::String(id)) => {
                                workspace_id = workspace_id.or(Some(id.as_str()));
                            }
                            ("at_ms" | "observed_at_ms", Value::Number(ms)) => {
                                at_ms = at_ms.or_else(|| ms.as_u64());
                            }
                            ("path", Value::String(path)) => paths.push(path.clone()),
                            ("paths", Value::Array(items)) => paths
                                .extend(items.iter().filter_map(Value::as_str).map(str::to_owned)),
                            // Other sessions' holdings, not this event's files.
                            ("concurrent_holders", _) => {}
                            _ => queue.push_back(field),
                        }
                    }
                }
                Value::Array(items) => queue.extend(items),
                _ => {}
            }
        }
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        DashboardEvent::new(recorded.seq, kind, session_id, workspace_id, at_ms, paths)
            .detail(&recorded.event)
    });
    DashboardEvents::new(after, rows.collect())
}

/// A page of the coordinator's event log, `/api/events` (issue #500).
fn events(context: &Context, query: &str) -> Reply {
    let (after, limit) = match feed_query(query) {
        Ok(asked) => asked,
        Err(message) => return Reply::text(400, &message),
    };
    let request = Request::new("read", Command::Events { after, limit });
    let (status, body) = match client::call(&context.state_dir, &request) {
        Ok(response) => match response.outcome {
            Outcome::Events { events } => {
                let page = feed_page(after, &events);
                (
                    200,
                    serde_json::json!({"available": true, "version": page.version,
                        "next": page.next, "events": page.events}),
                )
            }
            Outcome::Error { code, message } => (
                503,
                serde_json::json!({"available": false, "error": format!("{code}: {message}")}),
            ),
            _ => (
                503,
                serde_json::json!({"available": false, "error": "the coordinator answered with something other than events; restart the daemon after upgrading LangSpan"}),
            ),
        },
        Err(error) => (
            503,
            serde_json::json!({"available": false, "error": error.to_string()}),
        ),
    };
    json_reply(status, &body)
}

/// This local month's refusal count, the fields `langspan count --json`
/// prints with its label and the disclaimer (H5), or `{"error": ...}` when it cannot be read.
fn month_count(context: &Context) -> serde_json::Value {
    let answer = (|| -> Result<count::MonthCount, crate::Error> {
        let month = count::month(None, crate::store::wall_now_ms())?;
        let mut cached = context
            .month
            .lock()
            .map_err(|_| "the month cache was poisoned")?;
        let days = match cached.as_ref() {
            Some((seen, days)) if *seen == month => days.clone(),
            _ => {
                let days = count::day_starts(month)?;
                *cached = Some((month, days.clone()));
                days
            }
        };
        drop(cached);
        count::ask(&context.state_dir, month, days)
    })();
    match answer {
        Ok(answer) => {
            let label = count::Month::parse(&answer.month)
                .map(count::Month::label)
                .unwrap_or_default();
            let mut value = serde_json::to_value(&answer).unwrap_or_default();
            value["line"] = answer.summary(&label).into();
            value["label"] = label.into();
            value["note"] = count::DISCLAIMER.into();
            value
        }
        Err(error) => serde_json::json!({"error": error.to_string()}),
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
        for demo in [false, true] {
            let csp = content_security_policy(&page(demo).unwrap()).unwrap();
            assert!(!csp.contains("unsafe"), "{csp}");
            assert!(csp.contains("default-src 'none'"), "{csp}");
            assert!(csp.contains("img-src 'none'"), "{csp}");
            assert!(csp.contains("connect-src 'self'"), "{csp}");
            assert_eq!(csp.matches("'sha256-").count(), 2, "{csp}");
        }
    }

    /// The hash covers exactly the served block: one changed byte in it
    /// changes the policy.
    #[test]
    fn the_policy_hashes_follow_the_served_blocks() {
        let live = page(false).unwrap();
        let demo = page(true).unwrap();
        let script = |csp: &str| {
            csp.split("script-src ")
                .nth(1)
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        };
        let style = |csp: &str| {
            csp.split("style-src ")
                .nth(1)
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        };
        let (live_csp, demo_csp) = (
            content_security_policy(&live).unwrap(),
            content_security_policy(&demo).unwrap(),
        );
        assert_ne!(
            script(&live_csp),
            script(&demo_csp),
            "the demo script differs"
        );
        assert_eq!(style(&live_csp), style(&demo_csp), "one style serves both");
        let start = live.find("<script>").unwrap() + "<script>".len();
        let end = live.find("</script>").unwrap();
        let expected = format!(
            "'sha256-{}'",
            base64(&crate::sha256::digest(&live.as_bytes()[start..end]))
        );
        assert_eq!(script(&live_csp), expected);
    }

    /// The live page carries the renderer and never the simulated scenes;
    /// the demonstration carries both and says it is simulated.
    #[test]
    fn the_live_page_never_contains_the_demo_scenes() {
        let live = page(false).unwrap();
        let demo = page(true).unwrap();
        let sentinel = "langspan-demo-scenes-v1";
        assert!(DEMO_SCENES.contains(sentinel));
        for closing in ["</script", "<script", "<!--"] {
            assert!(
                !DEMO_SCENES.to_ascii_lowercase().contains(closing),
                "the scenes would end the script block early: {closing}"
            );
        }
        assert!(
            !live.contains(sentinel),
            "the live page loads demo fixtures"
        );
        assert!(demo.contains(sentinel));
        assert!(live.contains("const DEMO_SCENES = null;"));
        for composed in [&live, &demo] {
            assert!(!composed.contains(MARK_MARKER) && !composed.contains(DEMO_MARKER));
            assert_eq!(composed.matches("<script>").count(), 1);
            assert_eq!(composed.matches("<style>").count(), 1);
        }
        assert!(demo.contains("Simulated data"));
    }

    /// Quoted names the script passes to `call`, such as `byId('scope')`.
    fn quoted_arguments<'a>(script: &'a str, call: &str) -> Vec<&'a str> {
        script
            .match_indices(call)
            .filter_map(|(at, _)| {
                let rest = &script[at + call.len()..];
                rest.strip_prefix('\'')
                    .and_then(|rest| rest.split_once('\''))
                    .map(|(name, _)| name)
            })
            .collect()
    }

    /// Every element the script looks up and every icon it draws exists in
    /// the page, so a typo fails here instead of in a browser.
    #[test]
    fn every_element_and_icon_the_script_names_exists() {
        let page = page(true).unwrap();
        let script = &page[page.find("<script>").unwrap()..];
        let ids = quoted_arguments(script, "byId(");
        assert!(ids.len() > 30, "{ids:?}");
        for id in ids {
            assert!(
                page.contains(&format!("id=\"{id}\"")),
                "no element with id {id}"
            );
        }
        let mut icons = quoted_arguments(script, "icon(");
        // Icons named in tables of marks, records and panel titles.
        for table in [", '", "icon: '"] {
            for name in quoted_arguments(script, table) {
                if page.contains(&format!("data-icon=\"{name}\"")) {
                    icons.push(name);
                }
            }
        }
        for name in [
            "ban", "lock", "edit", "layers", "notice", "peer", "dot", "check", "fail", "warn",
            "land", "turn", "message", "note", "machine", "scope", "inspect", "health", "users",
            "file", "down", "right",
        ] {
            assert!(
                page.contains(&format!("data-icon=\"{name}\"")),
                "no icon {name}"
            );
        }
        for name in icons {
            assert!(
                page.contains(&format!("data-icon=\"{name}\"")),
                "no icon {name}"
            );
        }
    }

    /// The lockup is the approved kit's artwork, line for line after its
    /// header, in both themes; nothing is redrawn.
    #[test]
    fn the_brand_mark_is_the_kits_artwork_unchanged() {
        let live = page(false).unwrap();
        for file in [LOCKUP_LIGHT, LOCKUP_DARK] {
            let lines: Vec<&str> = file.trim().lines().skip(1).collect();
            assert!(lines.iter().filter(|line| line.contains("<path ")).count() >= 4);
            assert!(
                live.contains(&lines.join("\n")),
                "the artwork is embedded whole"
            );
        }
        assert!(live.contains("viewBox=\"0 0 288 80\""));
        assert!(live.contains("ls-mark-light") && live.contains("ls-mark-dark"));
        assert!(
            brand_mark("<svg>", "x").is_err(),
            "an unexpected file fails loudly"
        );
        assert!(
            brand_mark("<svg viewBox=\"0 0 1 1\">\n<path/>\n</svg>", "x").is_err(),
            "a header without the kit's namespace and fill fails loudly"
        );
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

    /// The `/api/events` page (issue #500), version 2 (issue #491): one row
    /// per stored event in seq order, each an allowlist projection. Prompt
    /// summaries, message text, task lines, publication payloads and input
    /// digests are never in it; an operation's time is its `observed_at_ms`.
    #[test]
    fn the_event_feed_keeps_ids_times_and_paths_and_drops_every_text() {
        use langspan_core::{DASHBOARD_PATHS, DASHBOARD_TEXT, RecordedEvent};
        let long = "d/".repeat(DASHBOARD_TEXT);
        let many: Vec<String> = (0..DASHBOARD_PATHS + 2)
            .map(|n| format!("f{n}.rs"))
            .collect();
        let mut leased = many.clone();
        leased[0] = long.clone();
        let events = [
            serde_json::json!({"type": "session_prompted", "session_id": "s1",
                "summary": "SECRET-PROMPT", "at_ms": 5, "generation": 1}),
            serde_json::json!({"type": "message_sent", "at_ms": 6, "message": {
                "message_id": "m1", "workspace_id": "work", "from": "s1",
                "to": {"kind": "everyone"}, "text": "SECRET-MESSAGE", "paths": ["a.rs"]}}),
            serde_json::json!({"type": "remote_published", "workspace_id": "work", "sequence": 1,
                "signature": "SECRET-SIGNATURE", "payload": "SECRET-PAYLOAD", "published_at_ms": 7}),
            serde_json::json!({"type": "intent_set", "intent": {"session_id": "s1",
                "workspace_id": "work", "paths": ["b.rs"], "summary": "SECRET-TASK", "generation": 1}}),
            serde_json::json!({"type": "lease_granted", "lease": {"lease_id": "l1",
                "session_id": "s1", "workspace_id": "work", "paths": leased,
                "generation": 1, "expires_at_ms": 9, "status": "active"}}),
            serde_json::json!({"type": "booted", "generation": 2}),
            serde_json::json!({"type": "write_refused", "session_id": "s2", "workspace_id": "work",
                "kind": "stale_base", "paths": ["b.rs"], "peer_session_id": "s1",
                "input_digest": "SECRET-DIGEST", "at_ms": 10, "generation": 2}),
            serde_json::json!({"type": "operation_observed", "observation": {
                "observation_id": "o1", "session_id": "s2", "workspace_id": "work",
                "operation_id": "op1", "source": "codex", "tool": "Edit", "paths": ["b.rs"],
                "result": "succeeded", "lease_id": "l2", "lease_relation": "current",
                "concurrent_holders": [{"lease_id": "l1", "session_id": "s1", "paths": ["c.rs"]}],
                "observed_at_ms": 11, "generation": 2}}),
        ];
        let recorded: Vec<RecordedEvent> = events
            .into_iter()
            .zip(41..)
            .map(|(event, seq)| RecordedEvent {
                seq,
                event: serde_json::from_value(event).unwrap(),
            })
            .collect();
        let page = serde_json::to_value(feed_page(40, &recorded)).unwrap();
        let mut clipped: String = long.chars().take(DASHBOARD_TEXT - 1).collect();
        clipped.push('…');
        let mut shown: Vec<String> = many[..DASHBOARD_PATHS].to_vec();
        shown[0] = clipped;
        assert_eq!(
            page,
            serde_json::json!({"version": 2, "next": 48, "events": [
                {"seq": 41, "type": "session_prompted", "session_id": "s1", "at_ms": 5,
                 "paths": [], "more_paths": 0},
                {"seq": 42, "type": "message_sent", "workspace_id": "work", "at_ms": 6,
                 "paths": ["a.rs"], "more_paths": 0},
                {"seq": 43, "type": "remote_published", "workspace_id": "work",
                 "paths": [], "more_paths": 0},
                {"seq": 44, "type": "intent_set", "session_id": "s1", "workspace_id": "work",
                 "paths": ["b.rs"], "more_paths": 0},
                {"seq": 45, "type": "lease_granted", "session_id": "s1", "workspace_id": "work",
                 "paths": shown, "more_paths": 2},
                {"seq": 46, "type": "booted", "paths": [], "more_paths": 0},
                {"seq": 47, "type": "write_refused", "session_id": "s2", "workspace_id": "work",
                 "at_ms": 10, "paths": ["b.rs"], "more_paths": 0,
                 "peer_session_id": "s1", "refusal": "stale_base"},
                {"seq": 48, "type": "operation_observed", "session_id": "s2",
                 "workspace_id": "work", "at_ms": 11, "paths": ["b.rs"], "more_paths": 0,
                 "tool": "Edit", "result": "succeeded"},
            ]})
        );
        assert!(!page.to_string().contains("SECRET"), "{page}");
        // An empty page resumes where it was asked from.
        assert_eq!(
            serde_json::to_value(feed_page(46, &[])).unwrap(),
            serde_json::json!({"version": 2, "next": 46, "events": []})
        );
    }

    #[test]
    fn the_event_feed_query_is_bounded_and_says_the_range() {
        assert_eq!(feed_query(""), Ok((0, 100)));
        assert_eq!(feed_query("t=x&after=7&limit=1000"), Ok((7, 1000)));
        for bad in ["after=-1", "after=x", "limit=0", "limit=1001", "limit="] {
            let error = feed_query(bad).unwrap_err();
            assert!(error.contains("between"), "{bad}: {error}");
        }
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
