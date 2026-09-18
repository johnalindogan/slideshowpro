//! Chromecast / Google TV content-cast spike.
//! Stack: mdns-sd discovery + rust_cast (Cast V2) + tiny_http LAN media server
//! → Default Media Receiver. Not chrome.cast / not desktop mirror.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use rust_cast::channels::media::{Media, StreamType};
use rust_cast::channels::receiver::CastDeviceApp;
use rust_cast::{CastDevice, ChannelMessage};
use serde::Serialize;
use tiny_http::{Header, Response, Server, StatusCode};
use uuid::Uuid;

const CAST_SERVICE: &str = "_googlecast._tcp.local.";
const DISCOVER_DEFAULT_MS: u64 = 8000;
const DISCOVER_MAX_MS: u64 = 10_000;

#[derive(Debug, Clone, Serialize)]
pub struct CastDeviceInfo {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub model: Option<String>,
}

/// Result of `cast_discover` — devices plus a short diagnostic for empty/error smoke.
#[derive(Debug, Clone, Serialize)]
pub struct CastDiscoverResult {
    pub devices: Vec<CastDeviceInfo>,
    /// Effective browse timeout (ms), after clamp.
    pub timeout_ms: u64,
    /// IPv4 interfaces used (or intended) for mDNS browse (name + ip).
    /// Prefer RFC1918 LAN; Tailscale / link-local are excluded when possible.
    pub interfaces: Vec<String>,
    /// mdns-sd / daemon error string if browse failed (no UUIDs/tokens/creds).
    pub error: Option<String>,
    /// One-line (or short) human summary for Cast UI status.
    pub diagnostic: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CastSessionInfo {
    pub device_name: String,
    pub device_host: String,
    pub device_port: u16,
    pub media_kind: String,
    pub media_url_host: String,
    pub http_port: u16,
}

#[derive(Default)]
pub struct CastState {
    inner: Mutex<Option<LiveSession>>,
}

struct LiveSession {
    cmd_tx: Sender<CastCmd>,
    info: Arc<Mutex<Option<CastSessionInfo>>>,
    _worker: Option<JoinHandle<()>>,
}

enum CastCmd {
    Load {
        kind: MediaKind,
        path: PathBuf,
        content_type: String,
        reply: Sender<Result<(), String>>,
    },
    Pause {
        reply: Sender<Result<(), String>>,
    },
    Play {
        reply: Sender<Result<(), String>>,
    },
    Disconnect {
        reply: Sender<Result<(), String>>,
    },
}

#[derive(Clone, Copy)]
enum MediaKind {
    Still,
    Video,
}

impl MediaKind {
    fn as_str(self) -> &'static str {
        match self {
            MediaKind::Still => "still",
            MediaKind::Video => "video",
        }
    }
}

/// Resolve demo/cast-spike assets (dev tree or Tauri resource bundle).
pub fn resolve_spike_asset(file_name: &str) -> Result<PathBuf, String> {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../demo/cast-spike").join(file_name),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/cast-spike").join(file_name),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("resources/cast-spike").join(file_name)))
            .unwrap_or_default(),
    ];
    for c in candidates {
        if c.is_file() {
            return c.canonicalize().map_err(|e| format!("canonicalize {}: {e}", c.display()));
        }
    }
    Err(format!(
        "spike asset not found: {file_name} (expected under demo/cast-spike/)"
    ))
}

fn is_under_spike_root(path: &Path) -> bool {
    let Ok(canon) = path.canonicalize() else {
        return false;
    };
    let roots = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../demo/cast-spike"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/cast-spike"),
    ];
    for r in roots {
        if let Ok(rr) = r.canonicalize() {
            if canon.starts_with(&rr) {
                return true;
            }
        }
    }
    false
}

fn assert_allowlisted_media(path: &Path) -> Result<PathBuf, String> {
    let canon = path
        .canonicalize()
        .map_err(|e| format!("media path not found: {e}"))?;
    if !is_under_spike_root(&canon) {
        return Err(
            "cast spike refuses path outside demo/cast-spike (no arbitrary FS / no BDO paths)"
                .into(),
        );
    }
    let ext = canon
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "mp4" | "webm" | "m4v" => Ok(canon),
        _ => Err(format!("unsupported media extension: .{ext}")),
    }
}

fn guess_lan_ipv4() -> Result<Ipv4Addr, String> {
    // UDP connect does not send packets; reveals the interface used toward the LAN/default route.
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
    sock.connect("8.8.8.8:80")
        .map_err(|e| format!("udp connect (for LAN IP): {e}"))?;
    match sock.local_addr().map_err(|e| e.to_string())?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Ok(v4),
        other => Err(format!("could not determine LAN IPv4 (got {other})")),
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// Label for Discover diagnostics: `Name (a.b.c.d)`.
fn iface_label(name: &str, v4: Ipv4Addr) -> String {
    format!("{name} ({v4})")
}

/// RFC1918 private LAN (10/8, 172.16–31/12, 192.168/16) — typical home Wi‑Fi.
fn is_rfc1918_v4(v4: Ipv4Addr) -> bool {
    v4.is_private()
}

/// 169.254/16 link-local (APIPA); Tailscale on Windows often shows here too.
fn is_link_local_v4(v4: Ipv4Addr) -> bool {
    v4.is_link_local()
}

/// 100.64.0.0/10 CGNAT — Tailscale's usual userspace range when not APIPA.
fn is_cgnat_tailscale_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (o[1] & 0xc0) == 64
}

fn is_tailscale_named(name: &str) -> bool {
    name.to_ascii_lowercase().contains("tailscale")
}

/// Skip Tailscale / link-local / CGNAT so Discover is not noisy when VPN is up.
fn should_skip_browse_iface(name: &str, v4: Ipv4Addr) -> bool {
    is_link_local_v4(v4) || is_cgnat_tailscale_v4(v4) || is_tailscale_named(name)
}

/// Prefer private LAN Wi‑Fi (RFC1918), excluding Tailscale / link-local.
fn is_preferred_browse_iface(name: &str, v4: Ipv4Addr) -> bool {
    is_rfc1918_v4(v4) && !should_skip_browse_iface(name, v4)
}

struct BrowseIfaces {
    /// Labels for ifaces we intend to browse on (preferred LAN, or fallback).
    browse: Vec<String>,
    browse_addrs: Vec<Ipv4Addr>,
    /// Labels briefly noted as skipped in the diagnostic.
    skipped: Vec<String>,
    skipped_addrs: Vec<Ipv4Addr>,
    /// True when browse list is RFC1918-preferred (not "everything left after skip").
    preferred_mode: bool,
}

/// Classify non-loopback IPv4 ifaces for mDNS browse preference.
fn classify_browse_ifaces() -> BrowseIfaces {
    let addrs = match if_addrs::get_if_addrs() {
        Ok(a) => a,
        Err(e) => {
            return BrowseIfaces {
                browse: vec![format!("(iface enum failed: {e})")],
                browse_addrs: vec![],
                skipped: vec![],
                skipped_addrs: vec![],
                preferred_mode: false,
            };
        }
    };

    let mut preferred: Vec<(String, Ipv4Addr)> = Vec::new();
    let mut skipped: Vec<(String, Ipv4Addr)> = Vec::new();
    let mut other: Vec<(String, Ipv4Addr)> = Vec::new();

    for i in addrs {
        if i.is_loopback() {
            continue;
        }
        let IpAddr::V4(v4) = i.ip() else {
            continue;
        };
        let label = iface_label(&i.name, v4);
        if should_skip_browse_iface(&i.name, v4) {
            skipped.push((label, v4));
        } else if is_preferred_browse_iface(&i.name, v4) {
            preferred.push((label, v4));
        } else {
            other.push((label, v4));
        }
    }

    preferred.sort_by(|a, b| a.0.cmp(&b.0));
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    other.sort_by(|a, b| a.0.cmp(&b.0));

    let skipped_labels: Vec<String> = skipped.iter().map(|(l, _)| l.clone()).collect();
    let skipped_addrs: Vec<Ipv4Addr> = skipped.iter().map(|(_, a)| *a).collect();

    if !preferred.is_empty() {
        BrowseIfaces {
            browse: preferred.iter().map(|(l, _)| l.clone()).collect(),
            browse_addrs: preferred.iter().map(|(_, a)| *a).collect(),
            skipped: skipped_labels,
            skipped_addrs,
            preferred_mode: true,
        }
    } else {
        // No RFC1918 Wi‑Fi — still exclude Tailscale/link-local; browse whatever remains.
        BrowseIfaces {
            browse: other.iter().map(|(l, _)| l.clone()).collect(),
            browse_addrs: other.iter().map(|(_, a)| *a).collect(),
            skipped: skipped_labels,
            skipped_addrs,
            preferred_mode: false,
        }
    }
}

fn iface_summary(interfaces: &[String]) -> String {
    if interfaces.is_empty() {
        "none".into()
    } else {
        interfaces.join(", ")
    }
}

fn discover_iface_diagnostic(pick: &BrowseIfaces) -> String {
    let browse = iface_summary(&pick.browse);
    if pick.skipped.is_empty() {
        format!("browse ifaces: [{browse}]")
    } else {
        let skipped = iface_summary(&pick.skipped);
        format!("browse ifaces: [{browse}]; skipped: [{skipped}]")
    }
}

/// Prefer RFC1918 LAN ifaces for mDNS; disable Tailscale / link-local / CGNAT.
fn apply_browse_iface_preference(daemon: &ServiceDaemon, pick: &BrowseIfaces) {
    if pick.preferred_mode && !pick.browse_addrs.is_empty() {
        // Bind browse to preferred LAN only (drop Tailscale / link-local / other).
        let _ = daemon.disable_interface(IfKind::All);
        let kinds: Vec<IfKind> = pick
            .browse_addrs
            .iter()
            .map(|a| IfKind::Addr(IpAddr::V4(*a)))
            .collect();
        let _ = daemon.enable_interface(kinds);
        return;
    }
    // Fallback: no RFC1918 — just disable noisy Tailscale / link-local / CGNAT.
    if !pick.skipped_addrs.is_empty() {
        let kinds: Vec<IfKind> = pick
            .skipped_addrs
            .iter()
            .map(|a| IfKind::Addr(IpAddr::V4(*a)))
            .collect();
        let _ = daemon.disable_interface(kinds);
    }
}

/// Discover Cast devices via mDNS. Caps at 10s.
/// Prefers RFC1918 LAN ifaces (e.g. 192.168.x Wi‑Fi); skips Tailscale / link-local / CGNAT.
/// Always returns a [`CastDiscoverResult`] (devices may be empty) so the UI can show
/// browse/skipped ifaces, the timeout used, and any mdns-sd error string.
pub fn discover_devices(timeout_ms: Option<u64>) -> Result<CastDiscoverResult, String> {
    let ms = timeout_ms
        .unwrap_or(DISCOVER_DEFAULT_MS)
        .min(DISCOVER_MAX_MS)
        .max(500);
    let pick = classify_browse_ifaces();
    let interfaces = pick.browse.clone();
    let iface_diag = discover_iface_diagnostic(&pick);

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            let err = format!("mdns daemon: {e}");
            let diagnostic = format!(
                "Discover FAIL: {err}. timeout {ms}ms; {iface_diag}"
            );
            eprintln!("[cast-spike] {diagnostic}");
            return Ok(CastDiscoverResult {
                devices: vec![],
                timeout_ms: ms,
                interfaces,
                error: Some(err),
                diagnostic,
            });
        }
    };
    apply_browse_iface_preference(&daemon, &pick);
    let receiver = match daemon.browse(CAST_SERVICE) {
        Ok(r) => r,
        Err(e) => {
            let _ = daemon.shutdown();
            let err = format!("mdns browse: {e}");
            let diagnostic = format!(
                "Discover FAIL: {err}. timeout {ms}ms; {iface_diag}"
            );
            eprintln!("[cast-spike] {diagnostic}");
            return Ok(CastDiscoverResult {
                devices: vec![],
                timeout_ms: ms,
                interfaces,
                error: Some(err),
                diagnostic,
            });
        }
    };

    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut found: HashMap<String, CastDeviceInfo> = HashMap::new();
    let mut last_recv_err: Option<String> = None;

    while Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        let wait = remain.min(Duration::from_millis(250));
        match receiver.recv_timeout(wait) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let addrs = info.get_addresses_v4();
                let host = addrs
                    .into_iter()
                    .next()
                    .map(|a| a.to_string())
                    .or_else(|| {
                        Some(info.get_hostname().trim_end_matches('.').to_string())
                            .filter(|s| !s.is_empty())
                    })
                    .unwrap_or_else(|| "0.0.0.0".to_string());
                let port = info.get_port();
                let name = info
                    .get_property_val_str("fn")
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        info.get_fullname()
                            .trim_end_matches('.')
                            .split('.')
                            .next()
                            .unwrap_or("Chromecast")
                            .to_string()
                    });
                let model = info.get_property_val_str("md").map(|s| s.to_string());
                let key = format!("{host}:{port}");
                found.insert(
                    key,
                    CastDeviceInfo {
                        name,
                        host,
                        port,
                        model,
                    },
                );
            }
            Ok(_) => {}
            Err(e) => {
                // Timeout is normal between events; Disconnected is noteworthy.
                let s = e.to_string();
                if !s.to_lowercase().contains("timeout") {
                    last_recv_err = Some(s);
                }
            }
        }
    }

    let _ = daemon.shutdown();
    let mut list: Vec<_> = found.into_values().collect();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let (error, diagnostic) = if list.is_empty() {
        let err = last_recv_err;
        let err_bit = err
            .as_ref()
            .map(|e| format!("; mdns: {e}"))
            .unwrap_or_default();
        let diagnostic = format!(
            "No Cast devices. mDNS {CAST_SERVICE} timeout {ms}ms; {iface_diag}{err_bit}"
        );
        (err, diagnostic)
    } else {
        (
            None,
            format!("{iface_diag}; timeout {ms}ms"),
        )
    };

    eprintln!(
        "[cast-spike] discover done in ≤{ms}ms → {} device(s); {}",
        list.len(),
        diagnostic
    );
    Ok(CastDiscoverResult {
        devices: list,
        timeout_ms: ms,
        interfaces,
        error,
        diagnostic,
    })
}

struct MediaHttp {
    port: u16,
    allow: Arc<Mutex<HashMap<String, PathBuf>>>,
    stop: Arc<AtomicBool>,
    _join: JoinHandle<()>,
}

impl MediaHttp {
    fn start() -> Result<Self, String> {
        // Bind 0.0.0.0 so Chromecast on LAN can reach us (not loopback-only).
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .map_err(|e| format!("http bind: {e}"))?;
        listener
            .set_nonblocking(false)
            .map_err(|e| format!("http set blocking: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("http local_addr: {e}"))?
            .port();
        // tiny_http wants ownership of the listener via from_listener.
        let server = Server::from_listener(listener, None).map_err(|e| format!("http server: {e}"))?;
        let allow: Arc<Mutex<HashMap<String, PathBuf>>> = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let allow_t = Arc::clone(&allow);
        let stop_t = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                match server.recv_timeout(Duration::from_millis(200)) {
                    Ok(Some(req)) => {
                        let url = req.url().to_string();
                        let token = url
                            .strip_prefix("/m/")
                            .map(|s| s.split('?').next().unwrap_or(s).to_string());
                        let path_opt = token.and_then(|t| {
                            allow_t
                                .lock()
                                .ok()
                                .and_then(|g| g.get(&t).cloned())
                        });
                        match path_opt {
                            Some(path) => match File::open(&path) {
                                Ok(mut f) => {
                                    let mut buf = Vec::new();
                                    if f.read_to_end(&mut buf).is_ok() {
                                        let ctype = content_type_for(&path);
                                        let mut resp = Response::from_data(buf);
                                        if let Ok(h) =
                                            Header::from_bytes("Content-Type", ctype)
                                        {
                                            resp.add_header(h);
                                        }
                                        let _ = req.respond(resp);
                                    } else {
                                        let _ = req.respond(Response::empty(StatusCode(500)));
                                    }
                                }
                                Err(_) => {
                                    let _ = req.respond(Response::empty(StatusCode(404)));
                                }
                            },
                            None => {
                                let _ = req.respond(Response::empty(StatusCode(404)));
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }
        });
        eprintln!("[cast-spike] LAN HTTP listening 0.0.0.0:{port} (allowlisted tokens only)");
        Ok(Self {
            port,
            allow,
            stop,
            _join: join,
        })
    }

    fn register(&self, path: PathBuf) -> Result<String, String> {
        let token = Uuid::new_v4().to_string().replace('-', "");
        self.allow
            .lock()
            .map_err(|_| "allowlist lock".to_string())?
            .insert(token.clone(), path);
        Ok(token)
    }

    fn clear(&self) {
        if let Ok(mut g) = self.allow.lock() {
            g.clear();
        }
    }

    fn shutdown(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.clear();
        // Join happens when MediaHttp drops after stop; give thread a moment.
        let _ = self._join.join();
        eprintln!("[cast-spike] LAN HTTP torn down");
    }
}

fn run_cast_worker(
    host: String,
    port: u16,
    device_name: String,
    cmd_rx: Receiver<CastCmd>,
    info_slot: Arc<Mutex<Option<CastSessionInfo>>>,
) {
    let http = match MediaHttp::start() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[cast-spike] http start failed: {e}");
            // Drain pending replies as errors.
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    CastCmd::Load { reply, .. }
                    | CastCmd::Pause { reply }
                    | CastCmd::Play { reply }
                    | CastCmd::Disconnect { reply } => {
                        let _ = reply.send(Err(e.clone()));
                    }
                }
            }
            return;
        }
    };
    let lan_ip = match guess_lan_ipv4() {
        Ok(ip) => ip,
        Err(e) => {
            eprintln!("[cast-spike] LAN IP failed: {e}");
            http.shutdown();
            return;
        }
    };

    let device = match CastDevice::connect_without_host_verification(host.as_str(), port) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[cast-spike] connect failed: {e}");
            http.shutdown();
            while let Ok(cmd) = cmd_rx.try_recv() {
                let reply = match cmd {
                    CastCmd::Load { reply, .. }
                    | CastCmd::Pause { reply }
                    | CastCmd::Play { reply }
                    | CastCmd::Disconnect { reply } => reply,
                };
                let _ = reply.send(Err(format!("cast connect: {e}")));
            }
            return;
        }
    };

    let app = CastDeviceApp::DefaultMediaReceiver;

    let launched = match device.receiver.launch_app(&app) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[cast-spike] launch DMR failed: {e}");
            http.shutdown();
            return;
        }
    };

    if let Err(e) = device.connection.connect(launched.transport_id.as_str()) {
        eprintln!("[cast-spike] transport connect: {e}");
        http.shutdown();
        return;
    }

    let transport_id = launched.transport_id.clone();
    let session_id = launched.session_id.clone();
    let mut media_session_id: Option<i32> = None;
    let mut running = true;

    while running {
        // Non-blocking-ish: try command with short timeout; also pump heartbeats.
        let cmd = cmd_rx.recv_timeout(Duration::from_millis(400));
        // Heartbeat / drain device messages
        loop {
            // rust_cast receive blocks; use a short attempt via try pattern unavailable —
            // send heartbeat pong opportunistically when we get messages during loads.
            break;
        }
        match cmd {
            Ok(CastCmd::Load {
                kind,
                path,
                content_type,
                reply,
            }) => {
                let result = (|| {
                    let path = assert_allowlisted_media(&path)?;
                    http.clear();
                    let token = http.register(path.clone())?;
                    let url = format!("http://{lan_ip}:{}/m/{token}", http.port);
                    let media = Media {
                        content_id: url.clone(),
                        stream_type: StreamType::Buffered,
                        content_type,
                        metadata: None,
                        duration: None,
                    };
                    let status = device
                        .media
                        .load(transport_id.as_str(), session_id.as_str(), &media)
                        .map_err(|e| format!("media load: {e}"))?;
                    if let Some(entry) = status.entries.first() {
                        media_session_id = Some(entry.media_session_id);
                    }
                    // Pump a few messages for heartbeat / status
                    for _ in 0..5 {
                        match device.receive() {
                            Ok(ChannelMessage::Heartbeat(_)) => {
                                let _ = device.heartbeat.pong();
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                    if let Ok(mut g) = info_slot.lock() {
                        *g = Some(CastSessionInfo {
                            device_name: device_name.clone(),
                            device_host: host.clone(),
                            device_port: port,
                            media_kind: kind.as_str().to_string(),
                            media_url_host: lan_ip.to_string(),
                            http_port: http.port,
                        });
                    }
                    eprintln!(
                        "[cast-spike] loaded {} → {}:{} (http :{})",
                        kind.as_str(),
                        host,
                        port,
                        http.port
                    );
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Ok(CastCmd::Pause { reply }) => {
                let result = (|| {
                    let sid = media_session_id.ok_or_else(|| "no media session".to_string())?;
                    device
                        .media
                        .pause(transport_id.as_str(), sid)
                        .map_err(|e| format!("pause: {e}"))?;
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Ok(CastCmd::Play { reply }) => {
                let result = (|| {
                    let sid = media_session_id.ok_or_else(|| "no media session".to_string())?;
                    device
                        .media
                        .play(transport_id.as_str(), sid)
                        .map_err(|e| format!("play: {e}"))?;
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Ok(CastCmd::Disconnect { reply }) => {
                if let Some(sid) = media_session_id {
                    let _ = device.media.stop(transport_id.as_str(), sid);
                }
                let _ = device.receiver.stop_app(session_id.as_str());
                running = false;
                let _ = reply.send(Ok(()));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Keep TLS alive with heartbeat if possible — try receive with care.
                // Blocking receive would stall commands; skip unless we can timeout.
                // rust_cast does not expose try_receive; rely on Cast device idle.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                running = false;
            }
        }
    }

    http.shutdown();
    if let Ok(mut g) = info_slot.lock() {
        *g = None;
    }
    eprintln!("[cast-spike] session worker exit (clean disconnect)");
}

impl CastState {
    fn ensure_session(
        &self,
        device: &CastDeviceInfo,
    ) -> Result<Sender<CastCmd>, String> {
        let mut guard = self.inner.lock().map_err(|_| "cast state lock".to_string())?;
        if let Some(live) = guard.as_ref() {
            let same = live
                .info
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|i| i.device_host == device.host))
                .unwrap_or(false);
            if same {
                return Ok(live.cmd_tx.clone());
            }
            // Different device — tear down first
            let old = guard.take().unwrap();
            let (reply_tx, reply_rx) = mpsc::channel();
            let _ = old.cmd_tx.send(CastCmd::Disconnect { reply: reply_tx });
            let _ = reply_rx.recv_timeout(Duration::from_secs(5));
            if let Some(h) = old._worker {
                let _ = h.join();
            }
        }

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let info_slot = Arc::new(Mutex::new(Some(CastSessionInfo {
            device_name: device.name.clone(),
            device_host: device.host.clone(),
            device_port: device.port,
            media_kind: "connecting".into(),
            media_url_host: String::new(),
            http_port: 0,
        })));
        let host = device.host.clone();
        let port = device.port;
        let name = device.name.clone();
        let info_slot_t = Arc::clone(&info_slot);
        let worker = thread::spawn(move || {
            run_cast_worker(host, port, name, cmd_rx, info_slot_t);
        });

        // Give worker a moment to connect
        thread::sleep(Duration::from_millis(300));
        *guard = Some(LiveSession {
            cmd_tx: cmd_tx.clone(),
            info: info_slot,
            _worker: Some(worker),
        });
        Ok(cmd_tx)
    }

    fn with_cmd<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&Sender<CastCmd>) -> Result<T, String>,
    {
        let guard = self.inner.lock().map_err(|_| "cast state lock".to_string())?;
        let live = guard.as_ref().ok_or_else(|| "no active cast session".to_string())?;
        f(&live.cmd_tx)
    }

    pub fn load_still(&self, device: &CastDeviceInfo) -> Result<CastSessionInfo, String> {
        let path = resolve_spike_asset("sample-still.jpg")?;
        let tx = self.ensure_session(device)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(CastCmd::Load {
            kind: MediaKind::Still,
            path,
            content_type: "image/jpeg".into(),
            reply: reply_tx,
        })
        .map_err(|_| "cast worker gone".to_string())?;
        reply_rx
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "cast still timed out".to_string())??;
        self.session_info()
    }

    pub fn load_video(&self, device: &CastDeviceInfo) -> Result<CastSessionInfo, String> {
        let path = resolve_spike_asset("sample-video.mp4")?;
        let tx = self.ensure_session(device)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(CastCmd::Load {
            kind: MediaKind::Video,
            path,
            content_type: "video/mp4".into(),
            reply: reply_tx,
        })
        .map_err(|_| "cast worker gone".to_string())?;
        reply_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "cast video timed out".to_string())??;
        self.session_info()
    }

    pub fn pause(&self) -> Result<(), String> {
        self.with_cmd(|tx| {
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(CastCmd::Pause { reply: reply_tx })
                .map_err(|_| "cast worker gone".to_string())?;
            reply_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| "pause timed out".to_string())?
        })
    }

    pub fn play(&self) -> Result<(), String> {
        self.with_cmd(|tx| {
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(CastCmd::Play { reply: reply_tx })
                .map_err(|_| "cast worker gone".to_string())?;
            reply_rx
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| "play timed out".to_string())?
        })
    }

    pub fn next(&self) -> Result<CastSessionInfo, String> {
        let info = self.session_info()?;
        let device = CastDeviceInfo {
            name: info.device_name.clone(),
            host: info.device_host.clone(),
            port: info.device_port,
            model: None,
        };
        // Prefer flipping still ↔ video based on current kind.
        if info.media_kind == "still" {
            self.load_video(&device)
        } else {
            self.load_still(&device)
        }
    }

    pub fn disconnect(&self) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "cast state lock".to_string())?;
        let Some(live) = guard.take() else {
            return Ok(());
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = live.cmd_tx.send(CastCmd::Disconnect { reply: reply_tx });
        let _ = reply_rx.recv_timeout(Duration::from_secs(8));
        if let Some(h) = live._worker {
            let _ = h.join();
        }
        Ok(())
    }

    pub fn session_info(&self) -> Result<CastSessionInfo, String> {
        let guard = self.inner.lock().map_err(|_| "cast state lock".to_string())?;
        let live = guard.as_ref().ok_or_else(|| "no active cast session".to_string())?;
        let info = live
            .info
            .lock()
            .map_err(|_| "cast info lock".to_string())?
            .clone();
        info.ok_or_else(|| "no active cast session".into())
    }
}

// Keep Write import used for clarity in docs; silence if unused on some toolchains.
#[allow(dead_code)]
fn _sink_write(w: &mut dyn Write, b: &[u8]) -> std::io::Result<()> {
    w.write_all(b)
}
