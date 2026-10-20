//! Browser preview for the Twinpane UI.
//!
//! Runs the REAL engine, but confined to a sandbox folder you choose, so you
//! can try the UI (and test it) without the desktop shell.
//!
//!     cargo run -p twinpane-dev -- --root /path/to/a/practice/folder
//!
//! Security model (a local web server that can change files is dangerous, so):
//!  * listens on 127.0.0.1 only;
//!  * every path is confined to `--root` (symlink escapes included);
//!  * API calls need a random per-run token that only the served page knows,
//!    which stops other websites from calling the API;
//!  * the Host and Origin headers are checked (blocks DNS-rebinding);
//!  * opening files with other programs is disabled.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};
use twinpane_core::api::Api;

const MAX_BODY: u64 = 10 * 1024 * 1024;

struct Config {
    root: PathBuf,
    port: u16,
    ui: PathBuf,
}

fn usage() -> ! {
    eprintln!("usage: twinpane-dev --root <folder> [--port 8731] [--ui <ui folder>]");
    exit(2)
}

fn parse_args() -> Config {
    let mut root = None;
    let mut port = 8731u16;
    let mut ui = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../ui"));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => root = args.next().map(PathBuf::from),
            "--port" => port = args.next().and_then(|p| p.parse().ok()).unwrap_or_else(|| usage()),
            "--ui" => ui = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let Some(root) = root else { usage() };
    Config { root, port, ui }
}

fn random_token() -> String {
    // RandomState is seeded from OS randomness.
    (0..4)
        .map(|_| {
            let mut h = RandomState::new().build_hasher();
            h.write_u8(1);
            format!("{:016x}", h.finish())
        })
        .collect()
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn security_headers<R: Read>(r: Response<R>) -> Response<R> {
    r.with_header(header(
        "Content-Security-Policy",
        "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'",
    ))
    .with_header(header("X-Content-Type-Options", "nosniff"))
    .with_header(header("Cache-Control", "no-store"))
}

fn text(status: u16, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    security_headers(
        Response::from_string(body)
            .with_status_code(status)
            .with_header(header("Content-Type", "text/plain; charset=utf-8")),
    )
}

fn get_header(rq: &Request, name: &str) -> Option<String> {
    rq.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn serve_static(rq: Request, url_path: &str, cfg: &Config, token: &str) {
    let rel = match url_path.trim_start_matches('/') {
        "" => "index.html",
        other => other,
    };
    if rel.contains("..") || rel.contains('\\') || rel.contains('\0') {
        let _ = rq.respond(text(400, "bad path"));
        return;
    }
    let ui_root = match std::fs::canonicalize(&cfg.ui) {
        Ok(p) => p,
        Err(_) => {
            let _ = rq.respond(text(500, "ui folder not found"));
            return;
        }
    };
    let file = match std::fs::canonicalize(ui_root.join(rel)) {
        Ok(p) if p.starts_with(&ui_root) && p.is_file() => p,
        _ => {
            let _ = rq.respond(text(404, "not found"));
            return;
        }
    };
    let Ok(mut bytes) = std::fs::read(&file) else {
        let _ = rq.respond(text(404, "not found"));
        return;
    };
    if file.file_name().map(|n| n == "index.html").unwrap_or(false) {
        let html = String::from_utf8_lossy(&bytes).replace(
            "<!--TWINPANE_TOKEN-->",
            &format!("<meta name=\"twinpane-token\" content=\"{token}\">"),
        );
        bytes = html.into_bytes();
    }
    let resp = security_headers(
        Response::from_data(bytes).with_header(header("Content-Type", content_type(&file))),
    );
    let _ = rq.respond(resp);
}

fn serve_api(mut rq: Request, cmd: &str, api: &Api) {
    let mut body = String::new();
    if rq
        .as_reader()
        .take(MAX_BODY)
        .read_to_string(&mut body)
        .is_err()
    {
        let _ = rq.respond(text(400, "unreadable body"));
        return;
    }
    let args: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                let _ = respond_json(rq, 400, json!({ "error": format!("bad JSON: {e}") }));
                return;
            }
        }
    };
    match api.call(cmd, args) {
        Ok(v) => {
            let _ = respond_json(rq, 200, json!({ "ok": v }));
        }
        Err(e) => {
            let _ = respond_json(rq, 400, json!({ "error": e }));
        }
    }
}

fn respond_json(rq: Request, status: u16, v: Value) -> std::io::Result<()> {
    rq.respond(security_headers(
        Response::from_string(v.to_string())
            .with_status_code(status)
            .with_header(header("Content-Type", "application/json")),
    ))
}

fn handle(rq: Request, api: Arc<Api>, cfg: Arc<Config>, token: Arc<String>) {
    let port = cfg.port;
    let allowed_hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    match get_header(&rq, "host") {
        Some(h) if allowed_hosts.contains(&h) => {}
        _ => {
            let _ = rq.respond(text(403, "bad host"));
            return;
        }
    }
    if let Some(origin) = get_header(&rq, "origin") {
        let ok = allowed_hosts
            .iter()
            .any(|h| origin == format!("http://{h}"));
        if !ok {
            let _ = rq.respond(text(403, "bad origin"));
            return;
        }
    }
    let url = rq.url().split('?').next().unwrap_or("/").to_string();
    match (rq.method().clone(), url.strip_prefix("/api/")) {
        (Method::Post, Some(cmd)) => {
            if get_header(&rq, "x-twinpane-token").as_deref() != Some(token.as_str()) {
                let _ = rq.respond(text(403, "bad token"));
                return;
            }
            let cmd = cmd.to_string();
            serve_api(rq, &cmd, &api);
        }
        (Method::Get, None) => serve_static(rq, &url, &cfg, &token),
        _ => {
            let _ = rq.respond(text(405, "method not allowed"));
        }
    }
}

fn main() {
    let cfg = parse_args();
    if !cfg.root.is_dir() {
        eprintln!("--root must be an existing folder: {}", cfg.root.display());
        exit(2);
    }
    let state_dir = std::env::temp_dir().join(format!("twinpane-preview-{}", cfg.port));
    let api = match Api::new(
        state_dir.join("holding"),
        Some(state_dir.join("undo.json")),
        Some(cfg.root.clone()),
    ) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("could not start: {e}");
            exit(1);
        }
    };
    let addr = format!("127.0.0.1:{}", cfg.port);
    let server = match Server::http(&addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not listen on {addr}: {e}");
            exit(1);
        }
    };
    let token = Arc::new(random_token());
    let cfg = Arc::new(cfg);
    println!("Twinpane preview: http://{addr}/");
    println!("Sandbox folder:   {}", cfg.root.display());
    println!("Deleted items are kept in: {}", state_dir.join("holding").display());
    for rq in server.incoming_requests() {
        let (api, cfg, token) = (api.clone(), cfg.clone(), token.clone());
        thread::spawn(move || handle(rq, api, cfg, token));
    }
}
