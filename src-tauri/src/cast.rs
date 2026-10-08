//! Chromecast / Google TV content cast.
//! Stack: mdns-sd discovery + rust_cast (Cast V2) + a LAN-only HTTP server
//! → Default Media Receiver. Not chrome.cast, not desktop mirror, no cloud relay.

use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent};
use rust_cast::channels::connection::ConnectionChannel;
use rust_cast::channels::heartbeat::{HeartbeatChannel, HeartbeatResponse};
use rust_cast::channels::media::{LoadOptions, Media, MediaChannel, MediaResponse, PlayerState, StreamType};
use rust_cast::channels::receiver::{CastDeviceApp, ReceiverChannel};
use rust_cast::message_manager::{CastMessage, MessageManager};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, StreamOwned,
    crypto::{aws_lc_rs::default_provider, verify_tls12_signature, verify_tls13_signature},
};
use serde::Serialize;
use uuid::Uuid;

const CAST_SERVICE: &str = "_googlecast._tcp.local.";
const DISCOVER_DEFAULT_MS: u64 = 8000;
const DISCOVER_MAX_MS: u64 = 10_000;
/// Inbound TCP range the installer opens on Private networks. Keep in sync with
/// `CAST_FW_TCP` in `src-tauri/windows/installer.nsi`.
pub const CAST_PORT_LO: u16 = 47200;
pub const CAST_PORT_HI: u16 = 47215;
const CAST_APP_ID_PORT: u16 = 8009;
const READ_IDLE: Duration = Duration::from_millis(350);
const READ_RPC: Duration = Duration::from_millis(900);
const LINK_DEAD_AFTER: Duration = Duration::from_secs(4);

pub const NO_TV_ERROR: &str = "No TV found on this network. Check that the PC and TV are on the same Wi-Fi, the firewall, or the router's client isolation.";

const UNSUPPORTED_MEDIA: &str =
    "Unsupported media. Cast plays JPEG, PNG, and MP4 (H.264 + AAC). Skipped.";

/// Shown when Windows has the home LAN marked Public, so Private-only firewall rules do not apply.
pub const PUBLIC_WIFI_MESSAGE: &str =
    "This Wi-Fi is set to Public in Windows. Set it to Private to cast.";

pub const MEDIA_LOAD_TIMEOUT_MESSAGE: &str =
    "The TV was found, but the photo or video did not load within 15 seconds.";

const HTTP_CONN_CAP: usize = 8;
const HTTP_CHUNK: usize = 64 * 1024;
/// No socket SO_RCVTIMEO/SO_SNDTIMEO. A Windows timeout leaves the socket undefined,
/// and a retried write can drop or repeat bytes. Stall with non-blocking WouldBlock instead.
const HTTP_POLL: Duration = Duration::from_millis(8);
const HTTP_IDLE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetCategory {
    Public,
    /// Private or DomainAuthenticated.
    Private,
    /// The category could not be read. Do not guess.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastNotice {
    NoTv,
    MediaLoadTimeout,
}

/// Public always uses the Windows Public message. Private and Unknown keep the scenario text.
pub fn cast_notice(scenario: CastNotice, category: NetCategory) -> String {
    if category == NetCategory::Public {
        return PUBLIC_WIFI_MESSAGE.to_string();
    }
    match scenario {
        CastNotice::NoTv => NO_TV_ERROR.to_string(),
        CastNotice::MediaLoadTimeout => MEDIA_LOAD_TIMEOUT_MESSAGE.to_string(),
    }
}

fn media_load_notice() -> String {
    cast_notice(CastNotice::MediaLoadTimeout, home_lan_category())
}

/// A cast that never starts on a Public network is the firewall, not a missing TV.
fn with_public_network(existing: String) -> String {
    if home_lan_category() == NetCategory::Public {
        PUBLIC_WIFI_MESSAGE.to_string()
    } else {
        existing
    }
}

fn home_lan_category() -> NetCategory {
    #[cfg(windows)]
    {
        let alias = snapshot_ifaces().bind.map(|b| b.name);
        return windows_lan_category(alias.as_deref());
    }
    #[cfg(not(windows))]
    {
        NetCategory::Unknown
    }
}

#[cfg(windows)]
fn windows_lan_category(alias: Option<&str>) -> NetCategory {
    let Some(alias) = alias else {
        return NetCategory::Unknown;
    };
    // Interface alias only (Wi-Fi, Ethernet). The SSID is a different property and is not read.
    let script = "Get-NetConnectionProfile | ForEach-Object { $_.InterfaceAlias + \"`t\" + [string]$_.NetworkCategory }";
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output();
    let Ok(output) = output else {
        return NetCategory::Unknown;
    };
    if !output.status.success() {
        return NetCategory::Unknown;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let Some((name, category)) = line.trim().rsplit_once('\t') else {
            continue;
        };
        if name.eq_ignore_ascii_case(alias) {
            return parse_net_category(category);
        }
    }
    NetCategory::Unknown
}

fn parse_net_category(raw: &str) -> NetCategory {
    let s = raw.trim().to_ascii_lowercase();
    if s == "public" {
        NetCategory::Public
    } else if s == "private" || s.starts_with("domain") {
        NetCategory::Private
    } else {
        NetCategory::Unknown
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CastDeviceInfo {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CastDiscoverResult {
    pub devices: Vec<CastDeviceInfo>,
    pub timeout_ms: u64,
    pub interfaces: Vec<String>,
    pub error: Option<String>,
    pub diagnostic: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CastConnectInfo {
    pub device_name: String,
    pub device_host: String,
    pub device_port: u16,
    pub bind_ip: String,
    pub http_port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistUpdate {
    pub registered: usize,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CastLiveStatus {
    pub connected: bool,
    pub player_state: String,
    pub remote_event: Option<String>,
    pub error: Option<String>,
    pub media_kind: String,
    pub http_port: u16,
    pub bind_ip: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CastFirewallStatus {
    pub state: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct IfaceCand {
    pub name: String,
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IfaceRole {
    Preferred,
    Skip,
}

/// Home-LAN addresses we will bind and browse. Public, Tailscale, and link-local are skipped.
pub fn iface_role(name: &str, ip: Ipv4Addr) -> IfaceRole {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
        return IfaceRole::Skip;
    }
    if is_link_local_v4(ip) || is_cgnat_v4(ip) || is_tailscale_named(name) {
        return IfaceRole::Skip;
    }
    if is_rfc1918_v4(ip) {
        IfaceRole::Preferred
    } else {
        IfaceRole::Skip
    }
}

pub fn is_rfc1918_v4(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

pub fn is_link_local_v4(ip: Ipv4Addr) -> bool {
    ip.is_link_local()
}

/// 100.64.0.0/10 — Tailscale's usual range.
pub fn is_cgnat_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xc0) == 64
}

pub fn is_tailscale_named(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("tailscale") || n.contains("zerotier")
}

pub fn is_safe_bind_ip(ip: Ipv4Addr) -> bool {
    iface_role("", ip) == IfaceRole::Preferred
}

fn prefix_rank(ip: Ipv4Addr) -> u8 {
    let o = ip.octets();
    if o[0] == 192 && o[1] == 168 {
        0
    } else if o[0] == 10 {
        1
    } else {
        2
    }
}

/// Pick the home-LAN address to bind. Prefers the default-route address when it is RFC1918,
/// otherwise a 192.168 address over 10/8 and 172.16/12. Never returns public, Tailscale, or 0.0.0.0.
pub fn choose_bind_ip<'a>(
    ifaces: &'a [IfaceCand],
    default_route: Option<Ipv4Addr>,
) -> Option<&'a IfaceCand> {
    let mut preferred: Vec<&IfaceCand> = ifaces
        .iter()
        .filter(|i| iface_role(&i.name, i.ip) == IfaceRole::Preferred)
        .collect();
    if preferred.is_empty() {
        return None;
    }
    if let Some(ip) = default_route {
        if let Some(hit) = preferred.iter().copied().find(|i| i.ip == ip) {
            return Some(hit);
        }
    }
    preferred.sort_by(|a, b| {
        prefix_rank(a.ip)
            .cmp(&prefix_rank(b.ip))
            .then(a.name.cmp(&b.name))
            .then(a.ip.cmp(&b.ip))
    });
    preferred.first().copied()
}

fn prefix_of(mask: Ipv4Addr) -> u8 {
    let bits = u32::from(mask).count_ones() as u8;
    if bits == 0 || bits > 30 {
        24
    } else {
        bits
    }
}

pub fn ipv4_in_subnet(ip: Ipv4Addr, network: Ipv4Addr, prefix: u8) -> bool {
    let prefix = prefix.clamp(1, 32);
    if prefix >= 32 {
        return ip == network;
    }
    let shift = 32 - prefix;
    (u32::from(ip) >> shift) == (u32::from(network) >> shift)
}

/// flume's idle `RecvTimeoutError::Timeout` displays as "timed out waiting on a channel".
/// That string does **not** contain "timeout", so a substring check for "timeout" treats every
/// quiet poll as a hard mDNS failure. Idle waits are not failures; a closed channel is.
pub fn is_mdns_idle_timeout(msg: &str) -> bool {
    let s = msg.to_ascii_lowercase();
    s.contains("timed out") || s.contains("timeout")
}

fn iface_label(name: &str, ip: Ipv4Addr) -> String {
    format!("{name} ({ip})")
}

struct LanSnapshot {
    bind: Option<IfaceCand>,
    preferred: Vec<IfaceCand>,
    browse: Vec<String>,
    skipped: Vec<String>,
    skipped_ips: Vec<Ipv4Addr>,
}

/// Stable description of the home-LAN adapters. The UI compares this and starts a
/// fresh mDNS browse when it changes. Discovery itself keeps no device cache.
pub fn network_fingerprint() -> String {
    let snap = snapshot_ifaces();
    fingerprint_of(&snap.preferred, snap.bind.as_ref().map(|b| b.ip))
}

fn fingerprint_of(preferred: &[IfaceCand], bind: Option<Ipv4Addr>) -> String {
    let mut parts: Vec<String> = preferred
        .iter()
        .map(|i| format!("{} {}/{}", i.name, i.ip, i.prefix))
        .collect();
    parts.sort();
    let bind_s = bind.map(|ip| ip.to_string()).unwrap_or_else(|| "-".into());
    format!("bind={bind_s}|{}", parts.join(","))
}

/// A resolved receiver is on the home LAN when it sits in any preferred adapter's
/// subnet. 2.4 GHz and 5 GHz on one router share that subnet; the band is not a filter.
fn device_on_home_lan(ip: Ipv4Addr, preferred: &[IfaceCand]) -> bool {
    if !is_safe_bind_ip(ip) {
        return false;
    }
    preferred
        .iter()
        .any(|c| ipv4_in_subnet(ip, c.ip, c.prefix))
}

fn snapshot_ifaces() -> LanSnapshot {
    let addrs = if_addrs::get_if_addrs().unwrap_or_default();
    let mut ifaces = Vec::new();
    for i in addrs {
        let if_addrs::IfAddr::V4(v4) = i.addr else {
            continue;
        };
        ifaces.push(IfaceCand {
            name: i.name,
            ip: v4.ip,
            prefix: prefix_of(v4.netmask),
        });
    }
    let route = default_route_v4().ok();
    let bind = choose_bind_ip(&ifaces, route).cloned();
    let preferred: Vec<IfaceCand> = ifaces
        .iter()
        .filter(|i| iface_role(&i.name, i.ip) == IfaceRole::Preferred)
        .cloned()
        .collect();
    let mut browse = Vec::new();
    let mut skipped = Vec::new();
    let mut skipped_ips = Vec::new();
    for i in &ifaces {
        let label = iface_label(&i.name, i.ip);
        match iface_role(&i.name, i.ip) {
            IfaceRole::Preferred => browse.push(label),
            IfaceRole::Skip => {
                if !i.ip.is_loopback() && !i.ip.is_unspecified() {
                    skipped_ips.push(i.ip);
                    skipped.push(label);
                }
            }
        }
    }
    browse.sort();
    skipped.sort();
    LanSnapshot {
        bind,
        preferred,
        browse,
        skipped,
        skipped_ips,
    }
}

fn default_route_v4() -> Result<Ipv4Addr, String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.connect("8.8.8.8:80").map_err(|e| e.to_string())?;
    match sock.local_addr().map_err(|e| e.to_string())?.ip() {
        IpAddr::V4(v4) => Ok(v4),
        other => Err(other.to_string()),
    }
}

fn no_tv_diagnostic(snap: &LanSnapshot, timeout_ms: u64, extra: Option<&str>) -> (String, String) {
    let notice = cast_notice(CastNotice::NoTv, home_lan_category());
    let browse = if snap.browse.is_empty() {
        "none".to_string()
    } else {
        snap.browse.join(", ")
    };
    let skipped = if snap.skipped.is_empty() {
        String::new()
    } else {
        format!(" Skipped interfaces: {}.", snap.skipped.join(", "))
    };
    let extra = extra
        .map(|e| format!(" Detail: {e}."))
        .unwrap_or_default();
    let lan = if snap.bind.is_none() {
        " This PC has no home-network address (VPN or Tailscale may be the only route)."
    } else {
        ""
    };
    let diagnostic = format!("{notice}{lan} Looked for {timeout_ms} ms on [{browse}].{skipped}{extra}");
    (notice, diagnostic)
}

fn empty_discover(
    snap: &LanSnapshot,
    timeout_ms: u64,
    interfaces: Vec<String>,
    extra: Option<&str>,
) -> CastDiscoverResult {
    let (notice, diagnostic) = no_tv_diagnostic(snap, timeout_ms, extra);
    CastDiscoverResult {
        devices: vec![],
        timeout_ms,
        interfaces,
        error: Some(notice),
        diagnostic,
    }
}

/// Discover Cast receivers. Returns within the timeout (hard cap 10s, always under 15s).
/// An empty network is a normal result with [`NO_TV_ERROR`], not an mDNS channel failure.
pub fn discover_devices(timeout_ms: Option<u64>) -> Result<CastDiscoverResult, String> {
    let ms = timeout_ms
        .unwrap_or(DISCOVER_DEFAULT_MS)
        .min(DISCOVER_MAX_MS)
        .max(500);
    let snap = snapshot_ifaces();
    let interfaces = snap.browse.clone();

    if snap.bind.is_none() && snap.browse.is_empty() {
        eprintln!("[cast] discover: no home-network adapter");
        return Ok(empty_discover(&snap, ms, interfaces, None));
    }

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[cast] discover: mDNS daemon failed to start");
            return Ok(empty_discover(
                &snap,
                ms,
                interfaces,
                Some(&format!("mDNS could not start ({e})")),
            ));
        }
    };

    let mut setup_notes: Vec<String> = Vec::new();
    // Do not disable IfKind::All. That drops every socket; if the following enable
    // does not land, browse listens nowhere and the UI used to blame a channel timeout.
    if let Err(e) = daemon.disable_interface(IfKind::IPv6) {
        let msg = e.to_string();
        if !is_mdns_idle_timeout(&msg) {
            setup_notes.push(format!("ipv6 filter: {msg}"));
        }
    }
    for ip in &snap.skipped_ips {
        if let Err(e) = daemon.disable_interface(IfKind::Addr(IpAddr::V4(*ip))) {
            let msg = e.to_string();
            if !is_mdns_idle_timeout(&msg) {
                setup_notes.push("skipped-interface filter failed".into());
                break;
            }
        }
    }
    for cand in &snap.preferred {
        if let Err(e) = daemon.enable_interface(IfKind::Addr(IpAddr::V4(cand.ip))) {
            let msg = e.to_string();
            if !is_mdns_idle_timeout(&msg) {
                setup_notes.push("could not pin browse to a home-network adapter".into());
                break;
            }
        }
    }

    let receiver = match daemon.browse(CAST_SERVICE) {
        Ok(r) => r,
        Err(e) => {
            let _ = daemon.shutdown();
            eprintln!("[cast] discover: browse failed to start");
            return Ok(empty_discover(
                &snap,
                ms,
                interfaces,
                Some(&format!("mDNS browse failed ({e})")),
            ));
        }
    };

    // Give the multicast join a moment before the deadline math, inside the same cap.
    thread::sleep(Duration::from_millis(200));
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut found: HashMap<String, CastDeviceInfo> = HashMap::new();
    let mut real_error: Option<String> = None;

    while Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        let wait = remain.min(Duration::from_millis(250));
        match receiver.recv_timeout(wait) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                // Do not read the device `id` TXT property (that is the Cast UUID).
                let port = info.get_port();
                let name = info
                    .get_property_val_str("fn")
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Chromecast".to_string());
                let model = info
                    .get_property_val_str("md")
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());
                for addr in info.get_addresses_v4() {
                    let ip = *addr;
                    if !device_on_home_lan(ip, &snap.preferred) {
                        continue;
                    }
                    let host = ip.to_string();
                    found.insert(
                        format!("{host}:{port}"),
                        CastDeviceInfo {
                            name: name.clone(),
                            host,
                            port,
                            model: model.clone(),
                        },
                    );
                }
            }
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                if is_mdns_idle_timeout(&msg) {
                    continue;
                }
                real_error = Some("mDNS browse stopped".into());
                break;
            }
        }
    }

    if let Ok(done) = daemon.shutdown() {
        let _ = done.recv_timeout(Duration::from_millis(400));
    }

    let mut list: Vec<_> = found.into_values().collect();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let extra = real_error.as_deref().or(setup_notes.first().map(String::as_str));
    let (error, diagnostic) = if list.is_empty() {
        let (notice, diagnostic) = no_tv_diagnostic(&snap, ms, extra);
        (Some(notice), diagnostic)
    } else {
        (
            None,
            format!(
                "Found {} on the home network ({} ms).",
                list.len(),
                ms
            ),
        )
    };
    eprintln!(
        "[cast] discover finished in ≤{ms} ms, {} device(s)",
        list.len()
    );
    Ok(CastDiscoverResult {
        devices: list,
        timeout_ms: ms,
        interfaces,
        error,
        diagnostic,
    })
}

pub fn parse_cast_host(host: &str, port: Option<u16>) -> Result<(Ipv4Addr, u16), String> {
    let ip: Ipv4Addr = host
        .trim()
        .parse()
        .map_err(|_| "Enter the TV's IPv4 address, for example 192.168.1.50.".to_string())?;
    if !is_safe_bind_ip(ip) {
        return Err(
            "That address is not on a home network. Cast stays on the LAN (no public IP, Tailscale, or link-local)."
                .into(),
        );
    }
    let port = port.unwrap_or(CAST_APP_ID_PORT);
    if port == 0 {
        return Err("Cast port must be between 1 and 65535.".into());
    }
    Ok((ip, port))
}

fn file_stem_label(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string()
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// JPEG, PNG, or MP4 (H.264 + AAC). Anything else is refused before it is tokenized.
pub fn classify_playlist_file(path: &Path) -> Result<(&'static str, String), String> {
    let canon = path
        .canonicalize()
        .map_err(|_| format!("{} is not available", file_stem_label(path)))?;
    if !canon.is_file() {
        return Err(format!("{} is not a file", file_stem_label(path)));
    }
    let ext = ext_of(&canon);
    let label = file_stem_label(&canon);
    match ext.as_str() {
        "jpg" | "jpeg" => {
            sniff_still(&canon, true)?;
            Ok(("image/jpeg", "still".into()))
        }
        "png" => {
            sniff_still(&canon, false)?;
            Ok(("image/png", "still".into()))
        }
        "mp4" | "m4v" => {
            if ext == "m4v" {
                return Err(format!("{label}: {UNSUPPORTED_MEDIA}"));
            }
            if !mp4_is_h264_aac(&canon).unwrap_or(false) {
                return Err(format!("{label}: {UNSUPPORTED_MEDIA}"));
            }
            Ok(("video/mp4", "video".into()))
        }
        _ => Err(format!("{label}: {UNSUPPORTED_MEDIA}")),
    }
}

fn sniff_still(path: &Path, jpeg: bool) -> Result<(), String> {
    let mut f = File::open(path).map_err(|_| format!("{} is not readable", file_stem_label(path)))?;
    let mut buf = [0u8; 8];
    let n = f.read(&mut buf).unwrap_or(0);
    let ok = if jpeg {
        n >= 3 && buf[0] == 0xff && buf[1] == 0xd8 && buf[2] == 0xff
    } else {
        n >= 8 && buf[..8] == *b"\x89PNG\r\n\x1a\n"
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{}: {UNSUPPORTED_MEDIA}", file_stem_label(path)))
    }
}

fn read_u32(f: &mut File) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    f.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_u64(f: &mut File) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    f.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

/// Walk MP4 boxes without reading `mdat`. True only when an `avc1`/`avc3` sample and an `mp4a` sample both exist.
pub fn mp4_is_h264_aac(path: &Path) -> std::io::Result<bool> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut saw_avc = false;
    let mut saw_aac = false;
    walk_boxes(&mut f, 0, len, 0, &mut saw_avc, &mut saw_aac)?;
    Ok(saw_avc && saw_aac)
}

fn walk_boxes(
    f: &mut File,
    start: u64,
    end: u64,
    depth: u8,
    saw_avc: &mut bool,
    saw_aac: &mut bool,
) -> std::io::Result<()> {
    if depth > 8 || *saw_avc && *saw_aac {
        return Ok(());
    }
    f.seek(SeekFrom::Start(start))?;
    while f.stream_position()? + 8 <= end {
        let pos = f.stream_position()?;
        let size32 = read_u32(f)? as u64;
        let mut typ = [0u8; 4];
        f.read_exact(&mut typ)?;
        let (size, header) = if size32 == 1 {
            let large = read_u64(f)?;
            (large, 16u64)
        } else if size32 == 0 {
            (end.saturating_sub(pos), 8u64)
        } else {
            (size32, 8u64)
        };
        if size < header {
            return Ok(());
        }
        let box_end = pos.saturating_add(size);
        if box_end > end || box_end <= pos {
            return Ok(());
        }
        if &typ == b"stsd" {
            parse_stsd(f, box_end, saw_avc, saw_aac)?;
        } else if matches!(&typ, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl") {
            let child = f.stream_position()?;
            walk_boxes(f, child, box_end, depth + 1, saw_avc, saw_aac)?;
        }
        f.seek(SeekFrom::Start(box_end))?;
        if *saw_avc && *saw_aac {
            return Ok(());
        }
    }
    Ok(())
}

fn parse_stsd(f: &mut File, box_end: u64, saw_avc: &mut bool, saw_aac: &mut bool) -> std::io::Result<()> {
    if f.stream_position()? + 8 > box_end {
        return Ok(());
    }
    let _ver_flags = read_u32(f)?;
    let count = read_u32(f)?.min(32);
    for _ in 0..count {
        if f.stream_position()? + 8 > box_end {
            break;
        }
        let entry_pos = f.stream_position()?;
        let entry_size = read_u32(f)? as u64;
        let mut four = [0u8; 4];
        f.read_exact(&mut four)?;
        match &four {
            b"avc1" | b"avc3" => *saw_avc = true,
            b"mp4a" => *saw_aac = true,
            _ => {}
        }
        let entry_end = if entry_size >= 8 {
            entry_pos.saturating_add(entry_size)
        } else {
            break;
        };
        if entry_end > box_end {
            break;
        }
        f.seek(SeekFrom::Start(entry_end))?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct AllowEntry {
    path: PathBuf,
    content_type: String,
    kind: String,
}

struct AllowBook {
    token: String,
    files: HashMap<String, AllowEntry>,
    by_path: HashMap<PathBuf, String>,
}

impl AllowBook {
    fn new() -> Self {
        Self {
            token: new_token(),
            files: HashMap::new(),
            by_path: HashMap::new(),
        }
    }

    fn revoke(&mut self) {
        self.files.clear();
        self.by_path.clear();
        self.token = new_token();
    }
}

fn new_token() -> String {
    Uuid::new_v4().simple().to_string()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Debug, PartialEq, Eq)]
enum Deny {
    NotFound,
}

/// Map `/m/<token>/<key>` to a playlist file. Rejects traversal, listing, and unknown tokens.
fn resolve_media_url<'a>(url: &str, book: &'a AllowBook) -> Result<&'a AllowEntry, Deny> {
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    if bare.contains('\\') || bare.contains('%') || bare.contains("..") || bare.contains("//") {
        return Err(Deny::NotFound);
    }
    let mut parts = bare.split('/').filter(|s| !s.is_empty());
    let root = parts.next();
    let token = parts.next();
    let key = parts.next();
    let extra = parts.next();
    if root != Some("m") || extra.is_some() {
        return Err(Deny::NotFound);
    }
    let (Some(token), Some(key)) = (token, key) else {
        return Err(Deny::NotFound);
    };
    if token.len() != book.token.len() || !ct_eq(token.as_bytes(), book.token.as_bytes()) {
        return Err(Deny::NotFound);
    }
    if key.len() > 80 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Deny::NotFound);
    }
    book.files.get(key).ok_or(Deny::NotFound)
}

fn replace_playlist(book: &mut AllowBook, paths: &[PathBuf]) -> PlaylistUpdate {
    book.files.clear();
    book.by_path.clear();
    let mut skipped = Vec::new();
    for path in paths {
        match classify_playlist_file(path) {
            Ok((ctype, kind)) => {
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                if book.by_path.contains_key(&canon) {
                    continue;
                }
                let key = new_token();
                book.by_path.insert(canon.clone(), key.clone());
                book.files.insert(
                    key,
                    AllowEntry {
                        path: canon,
                        content_type: ctype.to_string(),
                        kind,
                    },
                );
            }
            Err(reason) => skipped.push(reason),
        }
    }
    PlaylistUpdate {
        registered: book.files.len(),
        skipped,
    }
}

fn lookup_playlist<'a>(book: &'a AllowBook, path: &Path) -> Result<&'a AllowEntry, String> {
    let canon = path
        .canonicalize()
        .map_err(|_| "That slide is not in the current playlist.".to_string())?;
    let key = book
        .by_path
        .get(&canon)
        .ok_or_else(|| "That slide is not in the current playlist.".to_string())?;
    book.files
        .get(key)
        .ok_or_else(|| "That slide is not in the current playlist.".to_string())
}

struct MediaHttp {
    port: u16,
    bind_ip: Ipv4Addr,
    book: Arc<Mutex<AllowBook>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    inflight: Arc<AtomicUsize>,
}

impl MediaHttp {
    fn start_lan(ip: Ipv4Addr) -> Result<Self, String> {
        if !is_safe_bind_ip(ip) {
            return Err(
                "Refusing to listen: Cast only binds a home-network address, never 0.0.0.0, a public IP, or Tailscale."
                    .into(),
            );
        }
        Self::bind_on(ip, CAST_PORT_LO..=CAST_PORT_HI)
    }

    fn bind_on(ip: Ipv4Addr, ports: impl IntoIterator<Item = u16>) -> Result<Self, String> {
        Self::bind_with_idle(ip, ports, HTTP_IDLE)
    }

    fn bind_with_idle(
        ip: Ipv4Addr,
        ports: impl IntoIterator<Item = u16>,
        idle: Duration,
    ) -> Result<Self, String> {
        if ip.is_unspecified() {
            return Err("Refusing to listen on 0.0.0.0.".into());
        }
        let mut last = "no port in the Cast range was free".to_string();
        for port in ports {
            match TcpListener::bind(SocketAddr::from((ip, port))) {
                Ok(listener) => return Self::from_listener(listener, ip, idle),
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!(
            "Could not bind the Cast media server on {ip} ports {CAST_PORT_LO}-{CAST_PORT_HI} ({last})."
        ))
    }

    fn from_listener(listener: TcpListener, ip: Ipv4Addr, idle: Duration) -> Result<Self, String> {
        let port = listener
            .local_addr()
            .map_err(|e| format!("http local_addr: {e}"))?
            .port();
        let bound = listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .ip();
        if bound.is_unspecified() {
            return Err("Refusing to listen on 0.0.0.0.".into());
        }
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("http listen: {e}"))?;
        let book = Arc::new(Mutex::new(AllowBook::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let inflight = Arc::new(AtomicUsize::new(0));
        let book_t = Arc::clone(&book);
        let stop_t = Arc::clone(&stop);
        let workers_t = Arc::clone(&workers);
        let inflight_t = Arc::clone(&inflight);
        let join = thread::spawn(move || http_loop(listener, book_t, stop_t, workers_t, inflight_t, idle));
        eprintln!("[cast] media server listening on one LAN address, port {port}");
        Ok(Self {
            port,
            bind_ip: ip,
            book,
            stop,
            join: Some(join),
            workers,
            inflight,
        })
    }

    fn set_playlist(&self, paths: &[PathBuf]) -> Result<PlaylistUpdate, String> {
        let mut book = self.book.lock().map_err(|_| "playlist lock".to_string())?;
        Ok(replace_playlist(&mut book, paths))
    }

    fn media_url(&self, path: &Path) -> Result<(String, String), String> {
        let book = self.book.lock().map_err(|_| "playlist lock".to_string())?;
        let entry = lookup_playlist(&book, path)?;
        let key = book
            .by_path
            .get(&entry.path)
            .ok_or_else(|| "That slide is not in the current playlist.".to_string())?;
        let url = format!("http://{}:{}/m/{}/{}", self.bind_ip, self.port, book.token, key);
        Ok((url, entry.content_type.clone()))
    }

    fn kind_of(&self, path: &Path) -> Result<String, String> {
        let book = self.book.lock().map_err(|_| "playlist lock".to_string())?;
        Ok(lookup_playlist(&book, path)?.kind.clone())
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _busy = self.inflight.load(Ordering::SeqCst);
        if let Ok(mut book) = self.book.lock() {
            book.revoke();
        }
        // Join the accept thread first so it drops the listener and stops spawning.
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let workers = self
            .workers
            .lock()
            .ok()
            .map(|mut w| std::mem::take(&mut *w))
            .unwrap_or_default();
        for worker in workers {
            let _ = worker.join();
        }
        eprintln!("[cast] media server stopped and tokens revoked");
    }
}

impl Drop for MediaHttp {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn http_loop(
    listener: TcpListener,
    book: Arc<Mutex<AllowBook>>,
    stop: Arc<AtomicBool>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    inflight: Arc<AtomicUsize>,
    idle: Duration,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut sock, _)) => {
                // Non-blocking. Do not set SO_RCVTIMEO or SO_SNDTIMEO.
                if sock.set_nonblocking(true).is_err() {
                    continue;
                }
                let _ = sock.set_nodelay(true);
                if !claim_conn_slot(&inflight) {
                    // Dropping a socket that still holds the request makes Linux send RST
                    // and the client never sees the 503.
                    let _ = discard_unread(&mut sock);
                    let _ = write_empty(&mut sock, 503);
                    let _ = sock.shutdown(Shutdown::Write);
                    continue;
                }
                let book_c = Arc::clone(&book);
                let stop_c = Arc::clone(&stop);
                let inflight_c = Arc::clone(&inflight);
                let spawned = thread::Builder::new().name("cast-http".into()).spawn(move || {
                    struct Slot(Arc<AtomicUsize>);
                    impl Drop for Slot {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    let _slot = Slot(inflight_c);
                    let _ = serve_client(&mut sock, &book_c, &stop_c, idle);
                });
                match spawned {
                    Ok(handle) => {
                        if let Ok(mut list) = workers.lock() {
                            list.retain(|h| !h.is_finished());
                            list.push(handle);
                        }
                    }
                    Err(_) => {
                        inflight.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    // Dropping the listener here closes the port before `shutdown` returns.
    drop(listener);
}

fn claim_conn_slot(inflight: &AtomicUsize) -> bool {
    loop {
        let current = inflight.load(Ordering::SeqCst);
        if current >= HTTP_CONN_CAP {
            return false;
        }
        if inflight
            .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
    }
}

fn serve_client(
    sock: &mut TcpStream,
    book: &Mutex<AllowBook>,
    stop: &AtomicBool,
    idle: Duration,
) -> std::io::Result<()> {
    let mut pace = IoPace::new(idle);
    let raw = read_headers(sock, stop, &mut pace)?;
    let text = String::from_utf8_lossy(&raw);
    let mut lines = text.split("\r\n");
    let request = lines.next().unwrap_or("");
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let url = parts.next().unwrap_or("/").to_string();
    if method != "GET" && method != "HEAD" {
        return write_empty(sock, 405);
    }
    let mut range = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("range") {
                range = Some(value.trim().to_string());
            }
        }
    }
    let (token, key) = media_token_key(&url).unwrap_or_else(|| (String::new(), String::new()));
    let entry = {
        let guard = match book.lock() {
            Ok(g) => g,
            Err(_) => return write_empty(sock, 404),
        };
        match resolve_media_url(&url, &guard) {
            Ok(e) => e.clone(),
            Err(_) => return write_empty(sock, 404),
        }
    };
    let prepared = match transform_media(&entry) {
        Ok(body) => body,
        Err(_) => return write_empty(sock, 404),
    };
    let len = prepared.len;
    let ctype = prepared.content_type;
    let file = prepared.file;
    let head = method == "HEAD";
    if let Some(spec) = range {
        match parse_range(&spec, len) {
            Some((start, end)) => {
                write_file(sock, file, &ctype, start, end, len, head, stop, book, &token, &key, &mut pace)
            }
            None => write_empty(sock, 416),
        }
    } else if len == 0 {
        write_file(sock, file, &ctype, 0, 0, 0, head, stop, book, &token, &key, &mut pace)
    } else {
        write_file(sock, file, &ctype, 0, len - 1, len, head, stop, book, &token, &key, &mut pace)
    }
}

struct IoPace {
    last: Instant,
    idle: Duration,
}

impl IoPace {
    fn new(idle: Duration) -> Self {
        Self {
            last: Instant::now(),
            idle,
        }
    }

    fn advance(&mut self) {
        self.last = Instant::now();
    }

    fn stalled(&self) -> bool {
        self.last.elapsed() >= self.idle
    }
}

fn discard_unread(sock: &mut TcpStream) -> std::io::Result<()> {
    let mut buf = [0u8; 1024];
    loop {
        match sock.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn close_stalled(sock: &mut TcpStream) -> std::io::Error {
    // Write shutdown sends FIN so the TV's read returns. Drop closes the fd
    // when the worker returns, which frees the connection slot.
    let _ = discard_unread(sock);
    let _ = sock.shutdown(Shutdown::Write);
    std::io::Error::new(ErrorKind::TimedOut, "no forward progress")
}

fn io_wait(sock: &mut TcpStream, pace: &IoPace, stop: &AtomicBool) -> std::io::Result<()> {
    if stop.load(Ordering::SeqCst) {
        let _ = sock.shutdown(Shutdown::Both);
        return Err(std::io::Error::new(ErrorKind::ConnectionAborted, "cast stopped"));
    }
    if pace.stalled() {
        return Err(close_stalled(sock));
    }
    let remain = pace.idle.saturating_sub(pace.last.elapsed());
    thread::sleep(HTTP_POLL.min(remain));
    if stop.load(Ordering::SeqCst) {
        let _ = sock.shutdown(Shutdown::Both);
        return Err(std::io::Error::new(ErrorKind::ConnectionAborted, "cast stopped"));
    }
    if pace.stalled() {
        return Err(close_stalled(sock));
    }
    Ok(())
}

fn media_token_key(url: &str) -> Option<(String, String)> {
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    let mut parts = bare.split('/').filter(|s| !s.is_empty());
    if parts.next()? != "m" {
        return None;
    }
    Some((parts.next()?.to_string(), parts.next()?.to_string()))
}

fn grant_still_valid(book: &Mutex<AllowBook>, token: &str, key: &str) -> bool {
    let Ok(guard) = book.lock() else {
        return false;
    };
    token.len() == guard.token.len()
        && ct_eq(token.as_bytes(), guard.token.as_bytes())
        && guard.files.contains_key(key)
}

fn transfer_aborted(stop: &AtomicBool, book: &Mutex<AllowBook>, token: &str, key: &str) -> bool {
    stop.load(Ordering::SeqCst) || !grant_still_valid(book, token, key)
}

/// Bytes to send after the playlist token check.
///
/// This is the identity transform: the allowlisted file itself. A later slideshow
/// crop sidecar can replace a still here (new bytes and content type) without
/// changing the token check, the path check, or the socket loop.
struct PreparedMedia {
    file: File,
    len: u64,
    content_type: String,
}

fn transform_media(entry: &AllowEntry) -> std::io::Result<PreparedMedia> {
    let file = File::open(&entry.path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a file",
        ));
    }
    Ok(PreparedMedia {
        len: meta.len(),
        content_type: entry.content_type.clone(),
        file,
    })
}

fn read_headers(sock: &mut TcpStream, stop: &AtomicBool, pace: &mut IoPace) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(512);
    let mut tmp = [0u8; 512];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        match sock.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                pace.advance();
                buf.extend_from_slice(&tmp[..n]);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => io_wait(sock, pace, stop)?,
            Err(e) => return Err(e),
        }
    }
    Ok(buf)
}

fn write_empty(sock: &mut TcpStream, code: u16) -> std::io::Result<()> {
    let reason = match code {
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let msg = format!("HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let mut data = msg.as_bytes();
    let deadline = Instant::now() + Duration::from_millis(500);
    while !data.is_empty() {
        match sock.write(data) {
            Ok(0) => return Err(std::io::Error::new(ErrorKind::WriteZero, "closed")),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                thread::sleep(HTTP_POLL);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn write_all_checked(
    sock: &mut TcpStream,
    mut data: &[u8],
    stop: &AtomicBool,
    book: &Mutex<AllowBook>,
    token: &str,
    key: &str,
    pace: &mut IoPace,
) -> std::io::Result<()> {
    while !data.is_empty() {
        if transfer_aborted(stop, book, token, key) {
            let _ = sock.shutdown(Shutdown::Both);
            return Err(std::io::Error::new(ErrorKind::ConnectionAborted, "cast stopped"));
        }
        match sock.write(data) {
            Ok(0) => return Err(std::io::Error::new(ErrorKind::WriteZero, "closed")),
            Ok(n) => {
                // Only these bytes were accepted. A later WouldBlock retries the rest, never these.
                pace.advance();
                data = &data[n..];
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => io_wait(sock, pace, stop)?,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn write_file(
    sock: &mut TcpStream,
    mut file: File,
    ctype: &str,
    start: u64,
    end_inclusive: u64,
    total: u64,
    head: bool,
    stop: &AtomicBool,
    book: &Mutex<AllowBook>,
    token: &str,
    key: &str,
    pace: &mut IoPace,
) -> std::io::Result<()> {
    if transfer_aborted(stop, book, token, key) {
        let _ = sock.shutdown(Shutdown::Both);
        return Err(std::io::Error::new(ErrorKind::ConnectionAborted, "cast stopped"));
    }
    if total == 0 {
        let msg = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: 0\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
        );
        return write_all_checked(sock, msg.as_bytes(), stop, book, token, key, pace);
    }
    if start > end_inclusive || end_inclusive >= total {
        return write_empty(sock, 416);
    }
    let take = end_inclusive - start + 1;
    let partial = start != 0 || take != total;
    let status = if partial { 206 } else { 200 };
    let reason = if partial { "Partial Content" } else { "OK" };
    let mut msg = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {take}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n"
    );
    if partial {
        msg.push_str(&format!("Content-Range: bytes {start}-{end_inclusive}/{total}\r\n"));
    }
    msg.push_str("\r\n");
    write_all_checked(sock, msg.as_bytes(), stop, book, token, key, pace)?;
    if head {
        return Ok(());
    }
    file.seek(SeekFrom::Start(start))?;
    let mut left = take;
    let mut buf = vec![0u8; HTTP_CHUNK];
    while left > 0 {
        if transfer_aborted(stop, book, token, key) {
            let _ = sock.shutdown(Shutdown::Both);
            return Err(std::io::Error::new(ErrorKind::ConnectionAborted, "cast stopped"));
        }
        let chunk = left.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..chunk])?;
        if n == 0 {
            break;
        }
        write_all_checked(sock, &buf[..n], stop, book, token, key, pace)?;
        left -= n as u64;
    }
    Ok(())
}

/// `bytes=start-end`, `bytes=start-`, or `bytes=-suffix`. End is inclusive.
pub fn parse_range(spec: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let spec = spec.trim();
    let rest = spec
        .strip_prefix("bytes=")
        .or_else(|| spec.strip_prefix("bytes: "))?;
    let range = rest.split(',').next()?.trim();
    let (start_s, end_s) = range.split_once('-')?;
    if start_s.is_empty() {
        let suffix: u64 = end_s.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = size.saturating_sub(suffix);
        return Some((start, size - 1));
    }
    let start: u64 = start_s.parse().ok()?;
    if start >= size {
        return None;
    }
    let end = if end_s.is_empty() {
        size - 1
    } else {
        let end: u64 = end_s.parse().ok()?;
        end.min(size - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

#[derive(Debug)]
struct NoCertificateVerification;

impl ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

type CastStream = StreamOwned<ClientConnection, TcpStream>;

struct CastLink {
    tcp: TcpStream,
    mm: Rc<MessageManager<CastStream>>,
    heartbeat: HeartbeatChannel<'static, CastStream>,
    connection: ConnectionChannel<'static, CastStream>,
    receiver: ReceiverChannel<'static, CastStream>,
    media: MediaChannel<'static, CastStream>,
    transport_id: String,
    session_id: String,
}

impl CastLink {
    fn connect(host: Ipv4Addr, port: u16) -> Result<Self, String> {
        let addr = SocketAddr::from((host, port));
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(4)).map_err(|_| {
            format!(
                "TV unreachable at {host}. It may be off, or this PC and the TV are not on the same Wi-Fi."
            )
        })?;
        tcp.set_nodelay(true).ok();
        tcp.set_read_timeout(Some(Duration::from_secs(4))).ok();
        tcp.set_write_timeout(Some(Duration::from_secs(4))).ok();
        let tcp_ctl = tcp.try_clone().map_err(|e| format!("cast socket: {e}"))?;

        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
            .with_no_client_auth();
        let name = ServerName::try_from(host.to_string().as_str())
            .map_err(|_| "TV address is not a valid cast host.".to_string())?
            .to_owned();
        let conn = ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("cast tls: {e}"))?;
        let stream = StreamOwned::new(conn, tcp);
        let mm = Rc::new(MessageManager::new(stream));
        let heartbeat = HeartbeatChannel::new("sender-0", "receiver-0", Rc::clone(&mm));
        let connection = ConnectionChannel::new("sender-0", Rc::clone(&mm));
        let receiver = ReceiverChannel::new("sender-0", "receiver-0", Rc::clone(&mm));
        let media = MediaChannel::new("sender-0", Rc::clone(&mm));
        Ok(Self {
            tcp: tcp_ctl,
            mm,
            heartbeat,
            connection,
            receiver,
            media,
            transport_id: String::new(),
            session_id: String::new(),
        })
    }

    fn set_read_timeout(&self, dur: Duration) {
        self.tcp.set_read_timeout(Some(dur)).ok();
    }

    fn launch_receiver(&mut self) -> Result<(), String> {
        self.set_read_timeout(Duration::from_secs(4));
        let launched = self
            .receiver
            .launch_app(&CastDeviceApp::DefaultMediaReceiver)
            .map_err(|e| format!("Could not open the TV player ({e})."))?;
        self.connection
            .connect(launched.transport_id.as_str())
            .map_err(|e| format!("Could not connect to the TV player ({e})."))?;
        self.transport_id = launched.transport_id;
        self.session_id = launched.session_id;
        Ok(())
    }

    fn load_url(&self, url: &str, content_type: &str, autoplay: bool) -> Result<i32, String> {
        self.set_read_timeout(Duration::from_secs(15));
        let media = Media {
            content_id: url.to_string(),
            stream_type: StreamType::Buffered,
            content_type: content_type.to_string(),
            metadata: None,
            duration: None,
        };
        let status = self
            .media
            .load_with_opts(
                self.transport_id.as_str(),
                self.session_id.as_str(),
                &media,
                LoadOptions {
                    current_time: 0.0,
                    autoplay,
                },
            )
            .map_err(|e| map_load_error(&e.to_string()))?;
        let sid = status
            .entries
            .first()
            .map(|e| e.media_session_id)
            .ok_or_else(media_load_notice)?;
        let _ = content_type;
        Ok(sid)
    }

    fn pause(&self, sid: i32) -> Result<PlayerState, String> {
        self.set_read_timeout(READ_RPC);
        let entry = self
            .media
            .pause(self.transport_id.as_str(), sid)
            .map_err(|e| format!("Pause did not reach the TV ({e})."))?;
        Ok(entry.player_state)
    }

    fn play(&self, sid: i32) -> Result<PlayerState, String> {
        self.set_read_timeout(READ_RPC);
        let entry = self
            .media
            .play(self.transport_id.as_str(), sid)
            .map_err(|e| format!("Play did not reach the TV ({e})."))?;
        Ok(entry.player_state)
    }

    fn stop_app(&self, sid: Option<i32>) {
        self.set_read_timeout(Duration::from_millis(800));
        if let Some(sid) = sid {
            let _ = self.media.stop(self.transport_id.as_str(), sid);
        }
        if !self.session_id.is_empty() {
            let _ = self.receiver.stop_app(self.session_id.as_str());
        }
    }

    fn pump(&self) -> Result<Option<PlayerState>, String> {
        self.set_read_timeout(READ_IDLE);
        match self.mm.receive() {
            Ok(msg) => Ok(self.apply_message(&msg)),
            Err(e) if cast_err_is_timeout(&e) => Ok(None),
            Err(_) => Err(link_down_message()),
        }
    }

    fn apply_message(&self, msg: &CastMessage) -> Option<PlayerState> {
        if self.heartbeat.can_handle(msg) {
            if let Ok(HeartbeatResponse::Ping) = self.heartbeat.parse(msg) {
                let _ = self.heartbeat.pong();
            }
            return None;
        }
        if self.media.can_handle(msg) {
            if let Ok(MediaResponse::Status(status)) = self.media.parse(msg) {
                return status.entries.first().map(|e| e.player_state);
            }
            if let Ok(MediaResponse::LoadFailed(_)) = self.media.parse(msg) {
                return Some(PlayerState::Idle);
            }
        }
        None
    }

    fn ping(&self) -> Result<(), String> {
        self.heartbeat.ping().map_err(|_| link_down_message())
    }
}

fn map_load_error(err: &str) -> String {
    let l = err.to_ascii_lowercase();
    if l.contains("fail") || l.contains("invalid") || l.contains("not supported") {
        UNSUPPORTED_MEDIA.to_string()
    } else if is_cast_timeout(err) || l.contains("did not start") {
        media_load_notice()
    } else {
        format!("The TV did not take this slide ({err}).")
    }
}

fn cast_err_is_timeout(err: &rust_cast::errors::Error) -> bool {
    use std::io::ErrorKind;
    match err {
        rust_cast::errors::Error::Io(io) => {
            matches!(io.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
        }
        rust_cast::errors::Error::Timeout(_) => true,
        _ => {
            let l = err.to_string().to_ascii_lowercase();
            l.contains("timed out") || l.contains("would block") || l.contains("timeout")
        }
    }
}

fn is_cast_timeout(err: &str) -> bool {
    let l = err.to_ascii_lowercase();
    l.contains("timed out") || l.contains("would block") || l.contains("timeout")
}

fn link_down_message() -> String {
    "TV unreachable. It may be off or the Wi-Fi dropped. Cast again when it is back — you do not need to restart SlideX.".into()
}

fn player_name(state: PlayerState) -> &'static str {
    match state {
        PlayerState::Playing => "playing",
        PlayerState::Paused => "paused",
        PlayerState::Buffering => "buffering",
        PlayerState::Idle => "idle",
    }
}

struct SessionFlags {
    connected: bool,
    player_state: String,
    remote_event: Arc<Mutex<Option<String>>>,
    error: Option<String>,
    media_kind: String,
    local_cmd: Option<&'static str>,
    local_cmd_at: Instant,
    last_rx: Instant,
    media_session: Option<i32>,
}

impl SessionFlags {
    fn note_state(&mut self, state: PlayerState, from_local: bool) {
        let name = player_name(state);
        let prev = self.player_state.clone();
        self.player_state = name.to_string();
        if from_local || name == prev {
            return;
        }
        if name != "paused" && name != "playing" {
            return;
        }
        let matches_local = self.local_cmd == Some(if name == "paused" { "pause" } else { "play" })
            && self.local_cmd_at.elapsed() < Duration::from_secs(2);
        if !matches_local {
            if let Ok(mut ev) = self.remote_event.lock() {
                *ev = Some(name.to_string());
            }
        }
    }
}

enum CastCmd {
    SetPlaylist {
        paths: Vec<PathBuf>,
        reply: Sender<Result<PlaylistUpdate, String>>,
    },
    Load {
        path: PathBuf,
        autoplay: bool,
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

struct LiveSession {
    cmd_tx: Sender<CastCmd>,
    status: Arc<Mutex<CastLiveStatus>>,
    remote_event: Arc<Mutex<Option<String>>>,
    worker: Option<JoinHandle<()>>,
}

pub struct CastState {
    inner: Mutex<Option<LiveSession>>,
}

impl Default for CastState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }
}

impl CastState {
    pub fn connect(&self, host: &str, port: Option<u16>, name: Option<String>) -> Result<CastConnectInfo, String> {
        let (ip, port) = parse_cast_host(host, port)?;
        let snap = snapshot_ifaces();
        let bind = snap.bind.clone().ok_or_else(|| {
            "This PC has no home-network address to serve photos from. Disconnect VPN or Tailscale and try again.".to_string()
        })?;
        self.disconnect()?;

        let device_name = name.unwrap_or_else(|| "Chromecast".into());
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(CastLiveStatus {
            connected: false,
            player_state: "idle".into(),
            remote_event: None,
            error: None,
            media_kind: String::new(),
            http_port: 0,
            bind_ip: bind.ip.to_string(),
            }));
        let remote_event = Arc::new(Mutex::new(None));
        let status_t = Arc::clone(&status);
        let remote_t = Arc::clone(&remote_event);
        let friendly = device_name.clone();
        let worker = thread::Builder::new()
            .name("cast-session".into())
            .spawn(move || {
                run_session(ip, port, friendly, bind.ip, cmd_rx, ready_tx, status_t, remote_t);
            })
            .map_err(|e| format!("cast thread: {e}"))?;

        let info = match ready_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                let _ = worker.join();
                return Err(with_public_network(e));
            }
            Err(_) => {
                let _ = cmd_tx.send(CastCmd::Disconnect {
                    reply: mpsc::channel().0,
                });
                let _ = worker.join();
                return Err(with_public_network(link_down_message()));
            }
        };

        let mut guard = self.inner.lock().map_err(|_| "cast state lock".to_string())?;
        *guard = Some(LiveSession {
            cmd_tx,
            status,
            remote_event,
            worker: Some(worker),
        });
        Ok(info)
    }

    pub fn set_playlist(&self, paths: Vec<String>) -> Result<PlaylistUpdate, String> {
        let bufs = paths.iter().map(PathBuf::from).collect();
        self.roundtrip(|tx| {
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(CastCmd::SetPlaylist {
                paths: bufs,
                reply: reply_tx,
            })
            .map_err(|_| "Cast is not connected.".to_string())?;
            reply_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| "Updating the playlist on the TV timed out.".to_string())?
        })
    }

    pub fn load(&self, path: String, autoplay: bool) -> Result<(), String> {
        self.roundtrip(|tx| {
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(CastCmd::Load {
                path: PathBuf::from(path),
                autoplay,
                reply: reply_tx,
            })
            .map_err(|_| "Cast is not connected.".to_string())?;
            reply_rx
                .recv_timeout(Duration::from_secs(16))
                .map_err(|_| media_load_notice())?
        })
    }

    pub fn pause(&self) -> Result<(), String> {
        self.roundtrip(|tx| {
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(CastCmd::Pause { reply: reply_tx })
                .map_err(|_| "Cast is not connected.".to_string())?;
            reply_rx
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| link_down_message())?
        })
    }

    pub fn play(&self) -> Result<(), String> {
        self.roundtrip(|tx| {
            let (reply_tx, reply_rx) = mpsc::channel();
            tx.send(CastCmd::Play { reply: reply_tx })
                .map_err(|_| "Cast is not connected.".to_string())?;
            reply_rx
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| link_down_message())?
        })
    }

    pub fn disconnect(&self) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|_| "cast state lock".to_string())?;
        let Some(mut live) = guard.take() else {
            return Ok(());
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        let _ = live.cmd_tx.send(CastCmd::Disconnect { reply: reply_tx });
        let _ = reply_rx.recv_timeout(Duration::from_secs(3));
        if let Some(worker) = live.worker.take() {
            let _ = worker.join();
        }
        Ok(())
    }

    pub fn status(&self) -> CastLiveStatus {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => {
                return CastLiveStatus {
                    connected: false,
                    player_state: "idle".into(),
                    remote_event: None,
                    error: Some("Cast status is unavailable.".into()),
                    media_kind: String::new(),
                    http_port: 0,
                    bind_ip: String::new(),
                };
            }
        };
        let Some(live) = guard.as_ref() else {
            return CastLiveStatus {
                connected: false,
                player_state: "idle".into(),
                remote_event: None,
                error: None,
                media_kind: String::new(),
                http_port: 0,
                bind_ip: String::new(),
            };
        };
        let mut status = live
            .status
            .lock()
            .map(|g| g.clone())
            .unwrap_or(CastLiveStatus {
                connected: false,
                player_state: "idle".into(),
                remote_event: None,
                error: Some(link_down_message()),
                media_kind: String::new(),
                http_port: 0,
                bind_ip: String::new(),
            });
        status.remote_event = live.remote_event.lock().ok().and_then(|mut g| g.take());
        status
    }

    pub fn shutdown(&self) {
        let _ = self.disconnect();
    }

    fn roundtrip<T>(&self, f: impl FnOnce(&Sender<CastCmd>) -> Result<T, String>) -> Result<T, String> {
        let guard = self.inner.lock().map_err(|_| "cast state lock".to_string())?;
        let live = guard.as_ref().ok_or_else(|| "Cast is not connected.".to_string())?;
        f(&live.cmd_tx)
    }
}

impl Drop for CastState {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn publish(status: &Mutex<CastLiveStatus>, edit: impl FnOnce(&mut CastLiveStatus)) {
    if let Ok(mut g) = status.lock() {
        edit(&mut g);
    }
}

fn run_session(
    host: Ipv4Addr,
    port: u16,
    device_name: String,
    bind_ip: Ipv4Addr,
    cmd_rx: Receiver<CastCmd>,
    ready_tx: Sender<Result<CastConnectInfo, String>>,
    status: Arc<Mutex<CastLiveStatus>>,
    remote_event: Arc<Mutex<Option<String>>>,
) {
    let mut http = match MediaHttp::start_lan(bind_ip) {
        Ok(h) => h,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let mut link = match CastLink::connect(host, port) {
        Ok(l) => l,
        Err(e) => {
            http.shutdown();
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    if let Err(e) = link.launch_receiver() {
        http.shutdown();
        let _ = ready_tx.send(Err(e));
        return;
    }

    let info = CastConnectInfo {
        device_name: device_name.clone(),
        device_host: host.to_string(),
        device_port: port,
        bind_ip: bind_ip.to_string(),
        http_port: http.port,
    };
    publish(&status, |s| {
        s.connected = true;
        s.http_port = http.port;
        s.bind_ip = bind_ip.to_string();
        s.error = None;
    });
    let _ = ready_tx.send(Ok(info));
    eprintln!("[cast] session up, media port {}", http.port);

    let mut flags = SessionFlags {
        connected: true,
        player_state: "idle".into(),
        remote_event,
        error: None,
        media_kind: String::new(),
        local_cmd: None,
        local_cmd_at: Instant::now(),
        last_rx: Instant::now(),
        media_session: None,
    };
    let mut last_ping = Instant::now();
    let mut running = true;

    while running {
        if flags.last_rx.elapsed() > LINK_DEAD_AFTER {
            flags.connected = false;
            flags.error = Some(link_down_message());
            publish(&status, |s| {
                s.connected = false;
                s.error = Some(link_down_message());
            });
            fail_pending(&cmd_rx, &link_down_message());
            break;
        }

        match cmd_rx.try_recv() {
            Ok(cmd) => {
                if !handle_cmd(cmd, &mut http, &link, &mut flags, &status, &mut running) {
                    running = false;
                }
                flags.last_rx = Instant::now();
                continue;
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }

        if last_ping.elapsed() >= Duration::from_secs(2) {
            last_ping = Instant::now();
            if link.ping().is_err() {
                flags.connected = false;
                flags.error = Some(link_down_message());
                publish(&status, |s| {
                    s.connected = false;
                    s.error = Some(link_down_message());
                });
                fail_pending(&cmd_rx, &link_down_message());
                break;
            }
        }

        match link.pump() {
            Ok(Some(state)) => {
                flags.last_rx = Instant::now();
                flags.note_state(state, false);
                let name = flags.player_state.clone();
                publish(&status, |s| {
                    s.player_state = name;
                    s.connected = true;
                    s.error = None;
                });
            }
            Ok(None) => {}
            Err(e) => {
                flags.connected = false;
                flags.error = Some(e.clone());
                publish(&status, |s| {
                    s.connected = false;
                    s.error = Some(e.clone());
                });
                fail_pending(&cmd_rx, &e);
                break;
            }
        }
    }

    link.stop_app(flags.media_session);
    http.shutdown();
    publish(&status, |s| {
        s.connected = false;
        if s.error.is_none() {
            s.error = flags.error.clone();
        }
    });
    eprintln!("[cast] session ended");
}

fn fail_pending(cmd_rx: &Receiver<CastCmd>, err: &str) {
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            CastCmd::SetPlaylist { reply, .. } => {
                let _ = reply.send(Err(err.to_string()));
            }
            CastCmd::Load { reply, .. } | CastCmd::Pause { reply } | CastCmd::Play { reply } | CastCmd::Disconnect { reply } => {
                let _ = reply.send(Err(err.to_string()));
            }
        }
    }
}

fn handle_cmd(
    cmd: CastCmd,
    http: &mut MediaHttp,
    link: &CastLink,
    flags: &mut SessionFlags,
    status: &Mutex<CastLiveStatus>,
    running: &mut bool,
) -> bool {
    match cmd {
        CastCmd::SetPlaylist { paths, reply } => {
            let result = http.set_playlist(&paths);
            let _ = reply.send(result);
            true
        }
        CastCmd::Load { path, autoplay, reply } => {
            let result = (|| {
                let (url, ctype) = http.media_url(&path)?;
                let kind = http.kind_of(&path)?;
                let play = if kind == "still" { true } else { autoplay };
                let sid = link.load_url(&url, &ctype, play)?;
                flags.media_session = Some(sid);
                flags.media_kind = kind.clone();
                flags.player_state = if play { "playing" } else { "paused" }.into();
                flags.local_cmd = Some(if play { "play" } else { "pause" });
                flags.local_cmd_at = Instant::now();
                flags.last_rx = Instant::now();
                publish(status, |s| {
                    s.connected = true;
                    s.media_kind = kind;
                    s.player_state = flags.player_state.clone();
                    s.error = None;
                });
                eprintln!("[cast] loaded {}", flags.media_kind);
                Ok(())
            })();
            let link_dead = result
                .as_ref()
                .err()
                .is_some_and(|e: &String| e.contains("TV unreachable"));
            let _ = reply.send(result);
            if link_dead {
                flags.connected = false;
                publish(status, |s| {
                    s.connected = false;
                    s.error = Some(link_down_message());
                });
                return false;
            }
            true
        }
        CastCmd::Pause { reply } => {
            flags.local_cmd = Some("pause");
            flags.local_cmd_at = Instant::now();
            let result = (|| {
                let sid = flags.media_session.ok_or_else(|| "Nothing is casting.".to_string())?;
                if flags.media_kind == "still" {
                    flags.player_state = "paused".into();
                    publish(status, |s| s.player_state = "paused".into());
                    return Ok(());
                }
                let state = link.pause(sid)?;
                flags.note_state(state, true);
                flags.player_state = "paused".into();
                flags.last_rx = Instant::now();
                publish(status, |s| s.player_state = "paused".into());
                Ok(())
            })();
            let _ = reply.send(result);
            true
        }
        CastCmd::Play { reply } => {
            flags.local_cmd = Some("play");
            flags.local_cmd_at = Instant::now();
            let result = (|| {
                let sid = flags.media_session.ok_or_else(|| "Nothing is casting.".to_string())?;
                if flags.media_kind == "still" {
                    flags.player_state = "playing".into();
                    publish(status, |s| s.player_state = "playing".into());
                    return Ok(());
                }
                let state = link.play(sid)?;
                flags.note_state(state, true);
                flags.player_state = "playing".into();
                flags.last_rx = Instant::now();
                publish(status, |s| s.player_state = "playing".into());
                Ok(())
            })();
            let _ = reply.send(result);
            true
        }
        CastCmd::Disconnect { reply } => {
            *running = false;
            flags.connected = false;
            publish(status, |s| {
                s.connected = false;
                s.error = None;
            });
            let _ = reply.send(Ok(()));
            false
        }
    }
}

const FW_MEDIA_RULE: &str = "SlideX Cast media (Private)";
const FW_MDNS_RULE: &str = "SlideX Cast mDNS (Private)";

pub fn firewall_status() -> CastFirewallStatus {
    #[cfg(windows)]
    {
        let media = netsh_show(FW_MEDIA_RULE);
        let mdns = netsh_show(FW_MDNS_RULE);
        return parse_firewall_pair(&media, &mdns);
    }
    #[cfg(not(windows))]
    {
        CastFirewallStatus {
            state: "unknown".into(),
            detail: format!(
                "On Windows the installer adds \"{FW_MEDIA_RULE}\" and \"{FW_MDNS_RULE}\" for slideshowpro.exe: TCP {CAST_PORT_LO}-{CAST_PORT_HI} and UDP 5353, Private profile only. This check runs on Windows."
            ),
        }
    }
}

#[cfg(windows)]
fn netsh_show(name: &str) -> String {
    match std::process::Command::new("netsh")
        .args(["advfirewall", "firewall", "show", "rule", &format!("name={name}")])
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(e) => format!("query failed: {e}"),
    }
}

pub fn parse_firewall_pair(media: &str, mdns: &str) -> CastFirewallStatus {
    let media_ok = firewall_rule_ok(media, "TCP", &format!("{CAST_PORT_LO}-{CAST_PORT_HI}"));
    let mdns_ok = firewall_rule_ok(mdns, "UDP", "5353");
    if media_ok && mdns_ok {
        CastFirewallStatus {
            state: "added".into(),
            detail: format!(
                "Private firewall rules are installed for slideshowpro.exe: TCP {CAST_PORT_LO}-{CAST_PORT_HI} and UDP 5353."
            ),
        }
    } else if media_ok || mdns_ok {
        CastFirewallStatus {
            state: "partial".into(),
            detail: "Only part of the Cast firewall rule is installed. Re-run the SlideX installer and accept the Windows prompt.".into(),
        }
    } else if media.to_ascii_lowercase().contains("query failed") {
        CastFirewallStatus {
            state: "unknown".into(),
            detail: "Could not query the Windows firewall.".into(),
        }
    } else {
        CastFirewallStatus {
            state: "missing".into(),
            detail: format!(
                "No Private inbound rule for slideshowpro.exe. Cast needs TCP {CAST_PORT_LO}-{CAST_PORT_HI} and UDP 5353 on private networks. Re-run the installer and accept the Windows prompt."
            ),
        }
    }
}

fn firewall_rule_ok(text: &str, protocol: &str, ports: &str) -> bool {
    let l = text.to_ascii_lowercase();
    if l.contains("no rules match") || l.contains("query failed") {
        return false;
    }
    let private = l.contains("private") && !l.contains("public");
    // "Profiles: Private" is the success case. A rule that also lists Public is rejected
    // because the line contains "public".
    l.contains("slideshowpro.exe")
        && l.contains(&protocol.to_ascii_lowercase())
        && l.contains(&ports.to_ascii_lowercase())
        && private
        && l.contains("allow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cand(name: &str, ip: [u8; 4], prefix: u8) -> IfaceCand {
        IfaceCand {
            name: name.into(),
            ip: Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]),
            prefix,
        }
    }

    #[test]
    fn bind_prefers_home_wifi_over_tailscale_and_public() {
        let ifaces = vec![
            cand("Tailscale", [100, 64, 0, 5], 32),
            cand("Ethernet", [8, 8, 8, 8], 24),
            cand("Wi-Fi", [192, 168, 1, 20], 24),
            cand("vEthernet", [169, 254, 1, 1], 16),
            cand("Tailscale", [192, 168, 50, 2], 32),
        ];
        let pick = choose_bind_ip(&ifaces, Some(Ipv4Addr::new(100, 64, 0, 5))).unwrap();
        assert_eq!(pick.ip, Ipv4Addr::new(192, 168, 1, 20));
        assert!(iface_role("Tailscale", Ipv4Addr::new(100, 64, 0, 5)) == IfaceRole::Skip);
        assert!(iface_role("eth0", Ipv4Addr::new(169, 254, 3, 4)) == IfaceRole::Skip);
        assert!(iface_role("eth0", Ipv4Addr::UNSPECIFIED) == IfaceRole::Skip);
        assert!(!is_safe_bind_ip(Ipv4Addr::UNSPECIFIED));
        assert!(!is_safe_bind_ip(Ipv4Addr::new(1, 2, 3, 4)));
        assert!(is_safe_bind_ip(Ipv4Addr::new(10, 1, 2, 3)));
    }

    #[test]
    fn bind_uses_default_route_when_it_is_lan() {
        let ifaces = vec![
            cand("Wi-Fi", [192, 168, 1, 20], 24),
            cand("Ethernet", [10, 0, 0, 8], 24),
        ];
        let pick = choose_bind_ip(&ifaces, Some(Ipv4Addr::new(10, 0, 0, 8))).unwrap();
        assert_eq!(pick.ip, Ipv4Addr::new(10, 0, 0, 8));
    }

    #[test]
    fn no_lan_means_no_bind() {
        let ifaces = vec![
            cand("Tailscale", [100, 64, 1, 2], 32),
            cand("ppp", [172, 16, 0, 2], 32),
        ];
        // 172.16 is RFC1918 but the name is not tailscale. 172.16.0.2 is preferred.
        let pick = choose_bind_ip(&ifaces, None).unwrap();
        assert_eq!(pick.ip, Ipv4Addr::new(172, 16, 0, 2));
        let only_ts = vec![cand("Tailscale", [100, 64, 1, 2], 32)];
        assert!(choose_bind_ip(&only_ts, None).is_none());
    }

    #[test]
    fn subnet_match_and_idle_timeout_wording() {
        let net = Ipv4Addr::new(192, 168, 1, 20);
        assert!(ipv4_in_subnet(Ipv4Addr::new(192, 168, 1, 55), net, 24));
        assert!(!ipv4_in_subnet(Ipv4Addr::new(192, 168, 2, 55), net, 24));
        assert!(is_mdns_idle_timeout("timed out waiting on a channel"));
        assert!(is_mdns_idle_timeout("Timeout"));
        assert!(!is_mdns_idle_timeout("channel is empty and closed"));
        assert!(NO_TV_ERROR.contains("No TV found on this network"));
        assert!(NO_TV_ERROR.contains("same Wi-Fi"));
        assert!(NO_TV_ERROR.contains("firewall"));
        assert!(NO_TV_ERROR.contains("client isolation"));
        assert!(!NO_TV_ERROR.to_ascii_lowercase().contains("living room"));
        let home = cand("Wi-Fi", [192, 168, 1, 20], 24);
        assert!(device_on_home_lan(Ipv4Addr::new(192, 168, 1, 40), &[home.clone()]));
        assert!(!device_on_home_lan(Ipv4Addr::new(192, 168, 2, 40), &[home.clone()]));
        assert!(!device_on_home_lan(Ipv4Addr::new(8, 8, 8, 8), &[home.clone()]));
        let other = cand("Wi-Fi", [192, 168, 1, 21], 24);
        let fp_a = fingerprint_of(std::slice::from_ref(&home), Some(home.ip));
        let fp_b = fingerprint_of(std::slice::from_ref(&other), Some(other.ip));
        assert_ne!(fp_a, fp_b);
        assert_eq!(fp_a, fingerprint_of(std::slice::from_ref(&home), Some(home.ip)));
        assert!(parse_cast_host("192.168.1.40", None).is_ok());
        assert_eq!(parse_cast_host("  192.168.1.40  ", Some(8009)).unwrap().1, 8009);
    }

    #[test]
    fn refuses_unsafe_bind_without_listening() {
        assert!(MediaHttp::start_lan(Ipv4Addr::UNSPECIFIED).is_err());
        assert!(MediaHttp::start_lan(Ipv4Addr::new(8, 8, 8, 8)).is_err());
        assert!(MediaHttp::start_lan(Ipv4Addr::new(100, 64, 0, 1)).is_err());
        assert!(MediaHttp::start_lan(Ipv4Addr::new(169, 254, 1, 1)).is_err());
        assert!((CAST_PORT_LO..=CAST_PORT_HI).all(|p| (47200..=47215).contains(&p)));
        assert_eq!(CAST_PORT_HI - CAST_PORT_LO, 15);
    }

    fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("slidex-cast-{}-{}", std::process::id(), new_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    fn jpeg_bytes() -> Vec<u8> {
        vec![0xff, 0xd8, 0xff, 0xd9]
    }

    fn png_bytes() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&[0, 0, 0, 0]);
        v
    }

    fn mp4_with(entries: &[&[u8; 4]]) -> Vec<u8> {
        fn box_of(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let size = (8 + payload.len()) as u32;
            let mut out = size.to_be_bytes().to_vec();
            out.extend_from_slice(typ);
            out.extend_from_slice(payload);
            out
        }
        let mut stsd_payload = vec![0, 0, 0, 0];
        stsd_payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for four in entries {
            // 4 size + 4 fourcc + 8 reserved = 16, matching the size field.
            let entry_size: u32 = 16;
            stsd_payload.extend_from_slice(&entry_size.to_be_bytes());
            stsd_payload.extend_from_slice(four.as_slice());
            stsd_payload.extend_from_slice(&[0u8; 8]);
        }
        let stsd = box_of(b"stsd", &stsd_payload);
        let stbl = box_of(b"stbl", &stsd);
        let minf = box_of(b"minf", &stbl);
        let mdia = box_of(b"mdia", &minf);
        let trak = box_of(b"trak", &mdia);
        let moov = box_of(b"moov", &trak);
        let mut ftyp = box_of(b"ftyp", b"isom");
        // A large mdat must be skipped, not scanned, and must not hide the moov codecs.
        let mdat = box_of(b"mdat", &[0x61, 0x76, 0x63, 0x31, 0, 0, 0, 0]);
        ftyp.extend_from_slice(&mdat);
        ftyp.extend_from_slice(&moov);
        ftyp
    }

    #[test]
    fn playlist_gate_accepts_jpeg_png_h264_and_rejects_the_rest() {
        let jpg = scratch("a.jpg", &jpeg_bytes());
        let png = scratch("b.png", &png_bytes());
        let mp4 = scratch("c.mp4", &mp4_with(&[b"avc1", b"mp4a"]));
        let hevc = scratch("d.mp4", &mp4_with(&[b"hvc1", b"mp4a"]));
        let no_aac = scratch("e.mp4", &mp4_with(&[b"avc1"]));
        let webp = scratch("f.webp", b"RIFF");
        let mov = scratch("g.mov", &mp4_with(&[b"avc1", b"mp4a"]));
        assert!(classify_playlist_file(&jpg).is_ok());
        assert!(classify_playlist_file(&png).is_ok());
        assert!(classify_playlist_file(&mp4).is_ok());
        assert!(classify_playlist_file(&hevc).is_err());
        assert!(classify_playlist_file(&no_aac).is_err());
        assert!(classify_playlist_file(&webp).is_err());
        assert!(classify_playlist_file(&mov).is_err());
        assert!(mp4_is_h264_aac(&mp4).unwrap());
        assert!(!mp4_is_h264_aac(&hevc).unwrap());
    }

    #[test]
    fn token_allowlist_serves_only_playlist_files_and_blocks_traversal() {
        let jpg = scratch("slide.jpg", &jpeg_bytes());
        let other = scratch("secret.jpg", &jpeg_bytes());
        let png = scratch("two.png", &png_bytes());
        let http = MediaHttp::bind_on(Ipv4Addr::LOCALHOST, std::iter::once(0)).unwrap();
        assert_eq!(http.bind_ip, Ipv4Addr::LOCALHOST);
        assert_ne!(http.bind_ip, Ipv4Addr::UNSPECIFIED);
        let update = http.set_playlist(&[jpg.clone(), png.clone(), other.with_file_name("nope.webp")]).unwrap();
        assert_eq!(update.registered, 2);
        assert!(!update.skipped.is_empty());

        let (url_jpg, _) = http.media_url(&jpg).unwrap();
        let (url_png, _) = http.media_url(&png).unwrap();
        assert!(http.media_url(&other).is_err());
        let token = {
            let book = http.book.lock().unwrap();
            assert_eq!(book.token.len(), 32);
            assert!(!url_jpg.contains(&jpg.display().to_string()));
            assert!(url_jpg.contains(&book.token));
            book.token.clone()
        };
        assert_ne!(
            url_jpg.split('/').last().unwrap(),
            url_png.split('/').last().unwrap()
        );

        let (status, body) = http_get(http.bind_ip, http.port, &url_path(&url_jpg));
        assert_eq!(status, 200, "{body:?}");
        assert_eq!(body, jpeg_bytes());

        let (status, _) = http_get(http.bind_ip, http.port, "/");
        assert_eq!(status, 404);
        let (status, _) = http_get(http.bind_ip, http.port, "/m/");
        assert_eq!(status, 404);
        let (status, _) = http_get(http.bind_ip, http.port, &format!("/m/{token}"));
        assert_eq!(status, 404);
        let (status, _) = http_get(
            http.bind_ip,
            http.port,
            &format!("/m/{token}/{}", "deadbeef".repeat(4)),
        );
        assert_eq!(status, 404);
        let (status, _) = http_get(http.bind_ip, http.port, &format!("/m/{token}/../../etc/passwd"));
        assert_eq!(status, 404);
        let (status, _) = http_get(http.bind_ip, http.port, &format!("/m/{token}/%2e%2e/secret.jpg"));
        assert_eq!(status, 404);
        let (status, _) = http_get(
            http.bind_ip,
            http.port,
            &format!("/m/not-the-token/{}", url_jpg.split('/').last().unwrap()),
        );
        assert_eq!(status, 404);

        // Path is not a URL key. Using the filename must not serve the file.
        let (status, _) = http_get(http.bind_ip, http.port, &format!("/m/{token}/slide.jpg"));
        assert_eq!(status, 404);

        let (status, body) = http_get(http.bind_ip, http.port, &format!("{}?x=1", url_path(&url_png)));
        assert_eq!(status, 200);
        assert!(body.starts_with(b"\x89PNG"));

        // Range for video-style clients.
        let (status, hdr_body) = http_get_range(http.bind_ip, http.port, &url_path(&url_jpg), "bytes=1-2");
        assert_eq!(status, 206, "{hdr_body:?}");
        assert_eq!(hdr_body, vec![0xd8, 0xff]);

        // Revoke on disconnect: a new token and an empty book.
        {
            let mut book = http.book.lock().unwrap();
            book.revoke();
            assert!(book.files.is_empty());
            assert_ne!(book.token, token);
        }
        let (status, _) = http_get(http.bind_ip, http.port, &url_path(&url_jpg));
        assert_eq!(status, 404);

        let port = http.port;
        drop(http);
        let refused = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Duration::from_millis(400),
        );
        assert!(refused.is_err(), "media server must close when casting stops");
    }

    #[test]
    fn resolve_helper_rejects_listing_and_foreign_keys() {
        let mut book = AllowBook::new();
        let path = scratch("only.jpg", &jpeg_bytes());
        let update = replace_playlist(&mut book, &[path]);
        assert_eq!(update.registered, 1);
        let key = book.files.keys().next().unwrap().clone();
        let token = book.token.clone();
        assert!(resolve_media_url(&format!("/m/{token}/{key}"), &book).is_ok());
        assert!(resolve_media_url("/", &book).is_err());
        assert!(resolve_media_url(&format!("/m/{token}/{key}/extra"), &book).is_err());
        assert!(resolve_media_url(&format!("/m/{token}/../{key}"), &book).is_err());
        let mut other = AllowBook::new();
        assert!(resolve_media_url(&format!("/m/{}/{}", other.token, key), &book).is_err());
        other.revoke();
    }

    #[test]
    fn manual_ip_rejects_public_and_accepts_lan() {
        assert!(parse_cast_host("192.168.1.55", None).is_ok());
        assert!(parse_cast_host("8.8.8.8", Some(8009)).is_err());
        assert!(parse_cast_host("100.64.0.2", None).is_err());
        assert!(parse_cast_host("not-a-host", None).is_err());
        assert!(parse_cast_host("192.168.1.55", Some(0)).is_err());
    }

    #[test]
    fn cast_notice_covers_public_private_and_unknown_for_both_scenarios() {
        assert_ne!(PUBLIC_WIFI_MESSAGE, NO_TV_ERROR);
        assert_ne!(PUBLIC_WIFI_MESSAGE, MEDIA_LOAD_TIMEOUT_MESSAGE);
        assert_ne!(NO_TV_ERROR, MEDIA_LOAD_TIMEOUT_MESSAGE);

        assert_eq!(
            cast_notice(CastNotice::NoTv, NetCategory::Public),
            PUBLIC_WIFI_MESSAGE
        );
        assert_eq!(
            cast_notice(CastNotice::MediaLoadTimeout, NetCategory::Public),
            PUBLIC_WIFI_MESSAGE
        );
        assert_eq!(cast_notice(CastNotice::NoTv, NetCategory::Private), NO_TV_ERROR);
        assert_eq!(cast_notice(CastNotice::NoTv, NetCategory::Unknown), NO_TV_ERROR);
        assert_eq!(
            cast_notice(CastNotice::MediaLoadTimeout, NetCategory::Private),
            MEDIA_LOAD_TIMEOUT_MESSAGE
        );
        assert_eq!(
            cast_notice(CastNotice::MediaLoadTimeout, NetCategory::Unknown),
            MEDIA_LOAD_TIMEOUT_MESSAGE
        );

        assert_eq!(parse_net_category("Public"), NetCategory::Public);
        assert_eq!(parse_net_category("private"), NetCategory::Private);
        assert_eq!(parse_net_category("DomainAuthenticated"), NetCategory::Private);
        assert_eq!(parse_net_category(""), NetCategory::Unknown);
    }

    fn big_jpeg(len: usize) -> Vec<u8> {
        let mut v = vec![0xff, 0xd8, 0xff];
        v.resize(len, 0x11);
        v
    }

    fn hold_transfer(ip: Ipv4Addr, port: u16, path: &str) -> TcpStream {
        let mut slow = TcpStream::connect_timeout(&SocketAddr::from((ip, port)), Duration::from_secs(2)).unwrap();
        shrink_recv_buffer(&slow);
        slow.set_read_timeout(Some(Duration::from_millis(200))).ok();
        slow.set_nodelay(true).ok();
        let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        slow.write_all(req.as_bytes()).unwrap();
        slow
    }

    /// A small receive window keeps a large response blocked in `write`, instead of
    /// sitting entirely in the kernel buffer.
    fn shrink_recv_buffer(sock: &TcpStream) {
        let sz: i32 = 1024;
        #[cfg(unix)]
        unsafe {
            use std::os::fd::AsRawFd;
            extern "C" {
                fn setsockopt(sockfd: i32, level: i32, optname: i32, optval: *const u8, optlen: u32) -> i32;
            }
            // Linux SOL_SOCKET = 1, SO_RCVBUF = 8.
            let _ = setsockopt(sock.as_raw_fd(), 1, 8, &sz as *const i32 as *const u8, 4);
        }
        #[cfg(windows)]
        unsafe {
            use std::os::windows::io::AsRawSocket;
            extern "system" {
                fn setsockopt(s: usize, level: i32, optname: i32, optval: *const u8, optlen: i32) -> i32;
            }
            // Windows SOL_SOCKET = 0xffff, SO_RCVBUF = 0x1002.
            let _ = setsockopt(
                sock.as_raw_socket() as usize,
                0xffff,
                0x1002,
                &sz as *const i32 as *const u8,
                4,
            );
        }
    }

    #[test]
    fn slow_reader_does_not_block_the_next_request() {
        let big_path = scratch("big.jpg", &big_jpeg(2 * 1024 * 1024));
        let small_path = scratch("small.jpg", &jpeg_bytes());
        let http = MediaHttp::bind_on(Ipv4Addr::LOCALHOST, std::iter::once(0)).unwrap();
        http.set_playlist(&[big_path.clone(), small_path.clone()]).unwrap();
        let (url_big, _) = http.media_url(&big_path).unwrap();
        let (url_small, _) = http.media_url(&small_path).unwrap();
        let mut slow = hold_transfer(http.bind_ip, http.port, &url_path(&url_big));
        thread::sleep(Duration::from_millis(250));

        let started = Instant::now();
        let (status, body) = http_get(http.bind_ip, http.port, &url_path(&url_small));
        let elapsed = started.elapsed();
        assert_eq!(status, 200, "{body:?}");
        assert_eq!(body, jpeg_bytes());
        assert!(elapsed < Duration::from_secs(1), "second request took {elapsed:?}");

        slow.set_read_timeout(Some(Duration::from_millis(100))).ok();
        let mut got = 0usize;
        let mut buf = [0u8; 8192];
        if let Ok(n) = slow.read(&mut buf) {
            got += n;
        }
        assert!(
            got < 256 * 1024,
            "slow client should still be mid-transfer, read {got} bytes"
        );
    }

    #[test]
    fn shutdown_during_transfer_returns_within_one_second_and_closes_the_port() {
        let path = scratch("hold.jpg", &big_jpeg(2 * 1024 * 1024));
        let mut http = MediaHttp::bind_on(Ipv4Addr::LOCALHOST, std::iter::once(0)).unwrap();
        http.set_playlist(&[path.clone()]).unwrap();
        let (url, _) = http.media_url(&path).unwrap();
        let port = http.port;
        let ip = http.bind_ip;
        let _slow = hold_transfer(ip, port, &url_path(&url));
        thread::sleep(Duration::from_millis(150));
        let started = Instant::now();
        http.shutdown();
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(1), "shutdown took {elapsed:?}");
        let refused = TcpStream::connect_timeout(
            &SocketAddr::from((ip, port)),
            Duration::from_millis(400),
        );
        assert!(refused.is_err(), "port must be closed after shutdown");
    }

    #[test]
    fn revoked_token_is_404_including_mid_transfer() {
        let path = scratch("rev.jpg", &big_jpeg(2 * 1024 * 1024));
        let http = MediaHttp::bind_on(Ipv4Addr::LOCALHOST, std::iter::once(0)).unwrap();
        http.set_playlist(&[path.clone()]).unwrap();
        let (url, _) = http.media_url(&path).unwrap();
        let req_path = url_path(&url);
        let mut slow = hold_transfer(http.bind_ip, http.port, &req_path);
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let started = Instant::now();
        while got.len() < 1024 && started.elapsed() < Duration::from_secs(2) {
            match slow.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => continue,
                Err(_) => break,
            }
        }
        assert!(
            !got.is_empty() && got.len() < 2 * 1024 * 1024,
            "expected an in-progress body, got {} bytes",
            got.len()
        );
        {
            let mut book = http.book.lock().unwrap();
            book.revoke();
        }
        let before = got.len();
        let drain_until = Instant::now() + Duration::from_millis(800);
        while Instant::now() < drain_until && got.len() < 2 * 1024 * 1024 {
            match slow.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        assert!(
            got.len() < 512 * 1024,
            "revoked transfer must stop, had {before} bytes then {}",
            got.len()
        );
        let (status, _) = http_get(http.bind_ip, http.port, &req_path);
        assert_eq!(status, 404);
    }

    #[test]
    fn stalled_client_is_closed_after_idle_and_releases_its_slot() {
        assert_eq!(HTTP_IDLE, Duration::from_secs(30));
        let idle = Duration::from_millis(800);
        let big_path = scratch("idle.jpg", &big_jpeg(4 * 1024 * 1024));
        let small_path = scratch("idle-small.jpg", &jpeg_bytes());
        let http = MediaHttp::bind_with_idle(Ipv4Addr::LOCALHOST, std::iter::once(0), idle).unwrap();
        http.set_playlist(&[big_path.clone(), small_path.clone()]).unwrap();
        let (url_big, _) = http.media_url(&big_path).unwrap();
        let (url_small, _) = http.media_url(&small_path).unwrap();
        let _held: Vec<_> = (0..HTTP_CONN_CAP)
            .map(|_| hold_transfer(http.bind_ip, http.port, &url_path(&url_big)))
            .collect();
        thread::sleep(Duration::from_millis(200));
        let (busy, _) = http_get(http.bind_ip, http.port, &url_path(&url_small));
        assert_eq!(busy, 503, "eight stalled transfers should hold every connection slot");

        thread::sleep(idle + Duration::from_millis(400));
        let (status, body) = http_get(http.bind_ip, http.port, &url_path(&url_small));
        assert_eq!(status, 200, "{body:?}");
        assert_eq!(body, jpeg_bytes());
    }

    #[test]
    fn firewall_parser_requires_private_profile_and_ports() {
        let media = "\
Rule Name: SlideX Cast media (Private)
Enabled: Yes
Direction: In
Profiles: Private
LocalPort: 47200-47215
Protocol: TCP
Action: Allow
Program: C:\\Users\\qa\\AppData\\Local\\SlideShowX\\slideshowpro.exe
";
        let mdns = "\
Rule Name: SlideX Cast mDNS (Private)
Profiles: Private
LocalPort: 5353
Protocol: UDP
Action: Allow
Program: C:\\Users\\qa\\AppData\\Local\\SlideShowX\\slideshowpro.exe
";
        let ok = parse_firewall_pair(media, mdns);
        assert_eq!(ok.state, "added");
        let missing = parse_firewall_pair("No rules match the specified criteria.", "No rules match");
        assert_eq!(missing.state, "missing");
        let public_rule = media.replace("Profiles: Private", "Profiles: Public");
        let bad = parse_firewall_pair(&public_rule, mdns);
        assert_ne!(bad.state, "added");
    }

    fn url_path(url: &str) -> String {
        let rest = url.split("://").nth(1).unwrap();
        let path = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
        format!("/{path}")
    }

    fn http_get(ip: Ipv4Addr, port: u16, path: &str) -> (u16, Vec<u8>) {
        http_exchange(ip, port, &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"))
    }

    fn http_get_range(ip: Ipv4Addr, port: u16, path: &str, range: &str) -> (u16, Vec<u8>) {
        http_exchange(
            ip,
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nRange: {range}\r\nConnection: close\r\n\r\n"),
        )
    }

    fn http_exchange(ip: Ipv4Addr, port: u16, req: &str) -> (u16, Vec<u8>) {
        let mut sock = TcpStream::connect_timeout(&SocketAddr::from((ip, port)), Duration::from_secs(2)).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
        sock.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).unwrap();
        let text = String::from_utf8_lossy(&buf);
        let status: u16 = text
            .split_whitespace()
            .nth(1)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        let body = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| buf[i + 4..].to_vec())
            .unwrap_or_default();
        (status, body)
    }
}
