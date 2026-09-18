# SlideShowX Cast Spike — Chromecast / Google TV (content cast)

**Status:** spike / draft PR  
**Stack (LOCKED — Senior Dev GO):** **Rust Cast V2 + mDNS discovery + local LAN HTTP media server → Default Media Receiver**  
**Not used:** `chrome.cast` / Cast Web Sender (Chrome-only; does **not** work in Tauri WebView2)

## Why this stack

| Option | Verdict |
|--------|---------|
| `chrome.cast` / Cast Web Sender SDK | **FAIL for Tauri.** Requires Chrome Cast extension / Chrome-only APIs. WebView2 does not expose `chrome.cast`. Do not force it. |
| Desktop / screen mirror | **Out of scope.** Product requirement is **content cast** (still / video URLs), not mirroring the PC desktop. |
| Cloud relay / Cast SDK cloud | **Rejected.** LAN-only; no tokens/creds; no relay. |
| **Rust Cast V2 (`rust_cast`) + `mdns-sd` + allowlisted LAN HTTP (`tiny_http`)** | **Chosen.** Packages with Tauri 2 on Windows; PC stays controller; works with Default Media Receiver (`CC1AD845`). |

## Senior Dev bar

| # | Bar | Spike target |
|---|-----|--------------|
| 1 | LAN discovery ≤10s on home Wi‑Fi | mDNS browse `_googlecast._tcp.local.` (default timeout 8s, hard cap 10s) |
| 2 | Cast **one still** full-screen (content, not mirror) | Serve `demo/cast-spike/sample-still.jpg` over LAN HTTP → DMR `LOAD` |
| 3 | Cast **~30s video** OR FAIL-with-reason | Serve `demo/cast-spike/sample-video.mp4` (~30s) → DMR `LOAD` |
| 4 | PC controller: play/pause or next + clean disconnect | Tauri commands: pause / play / next / disconnect (stops app + tears down HTTP) |
| 5 | **One stack only** | This doc + code path only |

## Architecture

```
[SlideShowX Tauri UI]
        | invoke
[Rust commands: cast_discover / cast_load_* / cast_pause / cast_play / cast_next / cast_disconnect]
        |
   +----+----+
   |         |
[mdns-sd]  [Cast session worker]
 browse     rust_cast TLS → device:8009
            launch Default Media Receiver
            LOAD content_id = http://<LAN-IP>:<ephemeral>/m/<token>
                 |
           [tiny_http on 0.0.0.0:ephemeral]
            allowlist token → path ONLY
            tear down on disconnect
```

### Discovery flow

1. Frontend calls `cast_discover({ timeout_ms })` (default 8000, max 10000).
2. Rust starts `mdns-sd` ServiceDaemon, browses `_googlecast._tcp.local.`.
3. Collect unique `(ip, port, friendly_name, model)` until timeout.
4. Return list to UI (friendly name + ip:port). **Do not log device UUIDs / tokens.**

### Cast still / video flow

1. Resolve allowlisted sample under `demo/cast-spike/` (or resource bundle). No arbitrary FS; no BDO/bank paths.
2. Start (or reuse) LAN HTTP server bound to `0.0.0.0` (LAN-reachable). Register random opaque token → file path.
3. Compute PC LAN IPv4 (UDP connect trick / interface enum). Build `http://{lan}:{port}/m/{token}`.
4. `CastDevice::connect_without_host_verification(ip, port)` (Cast devices use self-signed certs).
5. `receiver.launch_app(Default)` → connect transport → `media.load(...)` with MIME `image/jpeg` or `video/mp4`.
6. Heartbeat thread replies to PING while session alive.
7. On disconnect: stop media/app, drop TLS session, **tear down HTTP server**, clear allowlist.

### Security locks (spike)

- HTTP serves **allowlisted token→path only**; unknown tokens → 404.
- Paths limited to spike samples (+ optional paths already in app `AllowedMedia`); reject `..` / absolute escapes.
- No credentials, OAuth, cloud relay, or device secrets in logs/PR body.
- Logs: friendly name + ip:port only when needed for smoke; no UUID dump.

## Failure modes

| Failure | Symptom | Mitigation / note |
|---------|---------|-------------------|
| PC & Chromecast on different Wi‑Fi / guest isolation | Discovery empty or cast stalls on HTTP fetch | Same SSID; disable AP/client isolation |
| Windows Firewall blocks inbound HTTP / mDNS | Discover OK but TV black / LOAD fails | Allow SlideShowX inbound on Private networks; UDP 5353 |
| mDNS blocked / VPN | Slow or empty discovery | Spike timeout ≤10s; document FAIL if empty |
| WebView2 `chrome.cast` | API undefined | Expected — native stack used instead |
| DHCP / IP conflict (living-room may share/conflict `.56`) | Stale IP after lease change | Re-run Discover before cast |
| Codec / MIME unsupported on receiver | Video FAIL | Prefer H.264 + AAC MP4 (sample-video.mp4); document FAIL with receiver error |
| Bound to 127.0.0.1 only | Chromecast cannot fetch media | Spike binds `0.0.0.0`; URL uses LAN IP |

## How to smoke (ALINX03 / ZB23 + Chromecast)

**Prereqs:** Windows build of this branch (`npm run build` / `tauri build` — do **not** overwrite John’s installed 0.1.4 NSIS unless QA uses a side-by-side/dev build). Chromecast / Google TV on same home LAN. Personal demo media only.

1. Launch spike build of SlideShowX.
2. Landing / viewer: open **Cast…** panel.
3. **Discover** — expect living-room (or any Cast device) within ≤10s.
4. Select device → **Cast still** — TV shows sample still full-screen (not PC desktop).
5. **Cast video** — ~30s sample plays; or note FAIL reason from UI/status.
6. **Pause** / **Play** (or **Next** to swap still↔video) from PC.
7. **Disconnect** — TV exits receiver; confirm HTTP port closed (no leftover listener).

CLI-oriented check (optional, same machine as app): after Discover, confirm mDNS sees `_googlecast._tcp`; during cast, from another LAN host `curl -I http://<pc-lan>:<port>/m/<token>` should 200 only for active token.

## Residual risks

- First-time Windows Firewall prompt may block inbound until allowed.
- Some Google TV builds are picky about Content-Type / Range requests; spike HTTP is minimal (may need Range for long seeks — out of scope for still + short clip).
- `rust_cast` uses host-verification-off for Cast TLS (industry-standard for LAN Cast).
- Multi-room / queue / DRM / AirPlay / phone sender: out of scope.
- Version stays **0.1.4** — no bump, no NSIS cut for John, no merge of this spike without review.

## App QA checklist

- [ ] Discover ≤10s lists Chromecast/Google TV  
- [ ] Still casts full-screen (content)  
- [ ] ~30s video casts **or** FAIL reason recorded here / in UI  
- [ ] Pause or Next from PC works  
- [ ] Disconnect cleans TV + tears down LAN HTTP  
- [ ] `npm run build` still produces NSIS (version unchanged 0.1.4)  
- [ ] No tokens/creds/BDO paths in logs or repo  


## Implementation status (this PR)

| Bar | Status |
|-----|--------|
| Stack locked in doc + code | GREEN — `rust_cast` + `mdns-sd` + `tiny_http` |
| Discover ≤10s | GREEN (code) — `cast_discover`; live LAN smoke on ALINX03/ZB23 |
| Cast one still | GREEN (code) — `cast_load_still` + sample-still.jpg |
| Cast ~30s video | GREEN (code) — `cast_load_video` + sample-video.mp4; confirm on living-room Chromecast |
| Pause / next / disconnect | GREEN (code) — commands + UI panel; HTTP torn down on disconnect |
| chrome.cast / WebView2 | Documented FAIL — not used |

UI: landing **Cast…** opens spike panel (Discover / Cast still / Cast video / Pause / Play / Next / Disconnect).
