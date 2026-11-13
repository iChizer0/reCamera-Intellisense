//! Clock repair and timezone discovery (`probe_timezone`, `sync_system_time`).
//!
//! Two facts shape this module:
//!
//! * Fixing the clock never requires knowing the timezone first. The clock is
//!   UTC seconds; the timezone only relabels display.
//! * Every probe here is broken-clock safe: NTP is plain UDP, HTTP `Date`
//!   headers and the geo-IP lookup use plain HTTP (a wrong clock breaks TLS
//!   certificate validation, so HTTPS probes would fail), WiFi country IEs
//!   (802.11d) are passive RF beacons, and DHCP timezone options (RFC 4833,
//!   options 100/101) come from the local broadcast domain. Each probe is a
//!   single bounded attempt — no retry storms.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use tokio::process::Command;

use crate::api_client::ApiClient;
use crate::types::DeviceRecord;

use super::system::{get_system_time, set_system_time, SetSystemTimeArgs};
use super::tzmap::timezones_for_country;

const DEFAULT_GEOIP_URL: &str = "http://ip-api.com/json/?fields=status,countryCode,timezone";
/// Plain-HTTP endpoints that answer with a `Date` header even when the client
/// clock is broken (coarse, TLS-independent time reference).
const DEFAULT_HTTP_DATE_URLS: &[&str] = &[
    "http://connectivitycheck.gstatic.com/generate_204",
    "http://www.msftconnecttest.com/connecttest.txt",
    "http://detectportal.firefox.com/success.txt",
];
const NTP_EPOCH_OFFSET: u64 = 2_208_988_800; // seconds between 1900 and 1970
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

// MARK: Low-level probes (bounded, broken-clock safe)

/// One SNTP round-trip over UDP; returns (unix_time, rtt_seconds).
/// Runs on a blocking thread with OS-level socket timeouts; callers should
/// wrap in `tokio::time::timeout` so a hung resolver can never wedge them
/// (the detached blocking task then just finishes in the background).
pub(crate) async fn sntp_query(server: &str, port: u16, timeout: Duration) -> Result<(f64, f64)> {
    let server = server.to_string();
    tokio::task::spawn_blocking(move || -> Result<(f64, f64)> {
        use std::net::ToSocketAddrs;
        let mut addrs = (server.as_str(), port).to_socket_addrs()?;
        let addr = addrs
            .next()
            .ok_or_else(|| anyhow::anyhow!("DNS lookup for {server} returned nothing"))?;
        let sock = UdpSocket::bind(if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
        sock.set_read_timeout(Some(timeout))?;
        sock.set_write_timeout(Some(timeout))?;
        let mut pkt = [0u8; 48];
        pkt[0] = 0x23; // LI=0, VN=4, Mode=3 (client)
        let t0 = Instant::now();
        sock.send_to(&pkt, addr)?;
        let mut buf = [0u8; 64];
        let (n, _) = sock.recv_from(&mut buf)?;
        let rtt = t0.elapsed().as_secs_f64();
        anyhow::ensure!(n >= 48, "short NTP response from {server} ({n} bytes)");
        let secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as u64;
        let frac = u32::from_be_bytes([buf[44], buf[45], buf[46], buf[47]]) as u64;
        anyhow::ensure!(secs > 0, "empty NTP timestamp from {server}");
        let ts = (secs - NTP_EPOCH_OFFSET) as f64 + frac as f64 / 4_294_967_296.0;
        Ok((ts, rtt))
    })
    .await?
}

/// Fetch the `Date` header of a plain-HTTP URL; returns (unix_time, rtt).
/// `https://` is refused on purpose: with a broken clock TLS validation fails,
/// which is exactly the failure mode this probe avoids.
pub(crate) async fn http_date(url: &str, timeout: Duration) -> Result<(f64, f64)> {
    anyhow::ensure!(
        url.to_ascii_lowercase().starts_with("http://"),
        "clock-safe time probe needs plain HTTP, got {url:?}"
    );
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let t0 = Instant::now();
    let resp = client
        .get(url)
        .header("User-Agent", "recamera-clockprobe/1")
        .send()
        .await?;
    let rtt = t0.elapsed().as_secs_f64();
    let date = resp
        .headers()
        .get(reqwest::header::DATE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let _ = resp.bytes().await;
    let date = date.ok_or_else(|| anyhow::anyhow!("no Date header in {url} response"))?;
    let ts = parse_http_date(&date).ok_or_else(|| anyhow::anyhow!("unparseable Date: {date:?}"))?;
    Ok((ts + rtt / 2.0, rtt))
}

/// Parse an IMF-fixdate (`Fri, 13 Nov 2026 14:22:46 GMT`) into unix seconds.
pub(crate) fn parse_http_date(s: &str) -> Option<f64> {
    let s = s.trim();
    let comma = s.find(',')?;
    let rest = s[comma + 1..].trim();
    let mut it = rest.split_whitespace();
    let day: u64 = it.next()?.parse().ok()?;
    let mon = match it.next()? {
        "Jan" => 1, "Feb" => 2, "Mar" => 3, "Apr" => 4, "May" => 5, "Jun" => 6,
        "Jul" => 7, "Aug" => 8, "Sep" => 9, "Oct" => 10, "Nov" => 11, "Dec" => 12,
        _ => return None,
    };
    let year: u64 = it.next()?.parse().ok()?;
    let hms = it.next()?;
    let mut hms_it = hms.split(':');
    let hour: u64 = hms_it.next()?.parse().ok()?;
    let min: u64 = hms_it.next()?.parse().ok()?;
    let sec: u64 = hms_it.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 || !(1970..=9999).contains(&year) {
        return None;
    }
    Some(days_from_civil(year, mon, day) as f64 * 86_400.0 + hour as f64 * 3600.0 + min as f64 * 60.0 + sec as f64)
}

/// Days since 1970-01-01 (Howard Hinnant's civil-from-days inverse).
fn days_from_civil(y: u64, m: u64, d: u64) -> i64 {
    let y = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = ((m as i64) + 9) % 12; // [0, 11], Mar=0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Plain-HTTP IP geolocation; works regardless of the local clock.
pub(crate) async fn geoip(url: &str, timeout: Duration) -> Result<Value> {
    anyhow::ensure!(
        url.to_ascii_lowercase().starts_with("http://"),
        "clock-safe geo-IP probe needs plain HTTP, got {url:?}"
    );
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let resp: Value = client
        .get(url)
        .header("User-Agent", "recamera-tzprobe/1")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // ip-api.com: {"status":"success","countryCode":"CN","timezone":"Asia/Shanghai"}
    // ipwho.is:   {"success":true,"country_code":"CN","timezone":{"id":"..."}}
    let (tz, cc) = if resp.get("status").and_then(|v| v.as_str()) == Some("success") {
        (
            resp.get("timezone").and_then(|v| v.as_str()).map(String::from),
            resp.get("countryCode").and_then(|v| v.as_str()).map(String::from),
        )
    } else if resp.get("success").and_then(|v| v.as_bool()) == Some(true) {
        (
            resp.pointer("/timezone/id").and_then(|v| v.as_str()).map(String::from),
            resp.get("country_code")
                .or_else(|| resp.get("countryCode"))
                .and_then(|v| v.as_str())
                .map(String::from),
        )
    } else {
        bail!("geo-IP probe failed: {}", resp.get("message").cloned().unwrap_or(resp.clone()));
    };
    let tz = tz.filter(|t| !t.is_empty()).context("geo-IP probe returned no timezone")?;
    Ok(json!({"timezone": tz, "country_code": cc, "url": url}))
}

/// Current UTC offset (seconds east of UTC) of an IANA zone via the system
/// `date` with TZ set — no tzdata dependency of our own. None when unknown.
pub(crate) fn utc_offset_seconds(tz_name: &str) -> Option<i64> {
    if !valid_timezone_name(tz_name) {
        return None;
    }
    let out = std::process::Command::new("date")
        .env("TZ", tz_name)
        .arg("+%z")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_tz_offset(&String::from_utf8_lossy(&out.stdout))
}

pub(crate) fn parse_tz_offset(s: &str) -> Option<i64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() != 5 || (bytes[0] != b'+' && bytes[0] != b'-') {
        return None;
    }
    let hours: i64 = s.get(1..3)?.parse().ok()?;
    let mins: i64 = s.get(3..5)?.parse().ok()?;
    let sign = if bytes[0] == b'-' { -1 } else { 1 };
    Some(sign * (hours * 3600 + mins * 60))
}

pub(crate) fn valid_timezone_name(tz: &str) -> bool {
    let tz = tz.trim();
    !tz.is_empty()
        && tz.len() <= 64
        && tz.is_ascii()
        && !tz.contains(char::is_whitespace)
        && !tz.contains("..")
        && tz.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
}

/// Majority-vote the 802.11d Country IEs of nearby APs (passive RF, on-device).
pub(crate) async fn wifi_countries() -> Result<BTreeMap<String, u32>> {
    let iw_out = Command::new("iw")
        .arg("dev")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let out = tokio::time::timeout(PROBE_TIMEOUT, iw_out).await??;
    let text = String::from_utf8_lossy(&out.stdout);
    let ifaces: Vec<String> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Interface ").map(str::to_string))
        .collect();
    let country_re = regex_country();
    let mut votes: BTreeMap<String, u32> = BTreeMap::new();
    for iface in ifaces {
        let scan = Command::new("iw")
            .args(["dev", &iface, "scan"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        if let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(20), scan).await {
            let text = String::from_utf8_lossy(&out.stdout);
            for l in text.lines() {
                let l = l.trim_start();
                if let Some(code) = country_re(l) {
                    if code != "00" && code != "99" {
                        // regulatory-domain "unset" sentinels carry no info
                        *votes.entry(code.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
    }
    if !votes.is_empty() {
        return Ok(votes);
    }
    // Fallback: the kernel regulatory domain, if an AP's country was adopted.
    let reg = Command::new("iw")
        .args(["reg", "get"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(Ok(out)) = tokio::time::timeout(PROBE_TIMEOUT, reg).await {
        let text = String::from_utf8_lossy(&out.stdout);
        for l in text.lines() {
            if let Some(rest) = l.trim_start().strip_prefix("country ") {
                if let Some(code) = rest.split(':').next() {
                    let code = code.trim();
                    if code.len() == 2 && code != "00" && code != "99" {
                        votes.insert(code.to_string(), 1);
                    }
                }
                break;
            }
        }
    }
    Ok(votes)
}

fn regex_country() -> impl Fn(&str) -> Option<&str> {
    // "Country: CN\tEnvironment: ..." -> "CN"; avoids pulling in a regex crate.
    |line| {
        let rest = line.strip_prefix("Country:")?.trim_start();
        let code = &rest[..2.min(rest.len())];
        if code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphanumeric()) {
            Some(code)
        } else {
            None
        }
    }
}

const UDHCP_PROBE_SCRIPT: &str = "#!/bin/sh\n# udhcpc event hook: dump the lease environment once, apply nothing.\nenv > \"$1\" 2>/dev/null || env > /tmp/recamera-dhcp-tz.env\nexit 0\n";

/// One-shot RFC 4833 probe: request DHCP options 100/101 (and 2), keep nothing.
/// The script only records the environment; the interface is never touched.
pub(crate) async fn dhcp_tz_probe(iface: &str, timeout: Duration) -> Result<Value> {
    let dir = std::env::temp_dir().join(format!("recamera-tzprobe-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let env_path = dir.join("lease.env");
    let script_path = dir.join("probe.sh");
    let script = UDHCP_PROBE_SCRIPT.replace("$1", &env_path.display().to_string());
    std::fs::write(&script_path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
    }
    let run = Command::new("udhcpc")
        .args([
            "-i", iface, "-n", "-q", "-t", "2", "-T", "2",
            "-o", "-O", "100", "-O", "101", "-O", "2",
            "-s", &script_path.display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let _ = tokio::time::timeout(timeout, run)
        .await
        .context("DHCP probe timed out")??;
    let text = std::fs::read_to_string(&env_path)
        .context(format!("DHCP probe on {iface} obtained no lease options"))?;
    let env: BTreeMap<String, String> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Ok(parse_dhcp_env(&env))
}

/// udhcpc exports unknown options as hex (or raw); both decode to ASCII.
fn decode_dhcp_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.len().is_multiple_of(2) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        let raw: Option<Vec<u8>> = (0..value.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
            .collect();
        if let Some(raw) = raw {
            if let Ok(text) = String::from_utf8(raw) {
                if text.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                    return Some(text);
                }
            }
        }
    }
    if value.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        Some(value.to_string())
    } else {
        None
    }
}

/// Extract RFC 4833 PCode/TCode (+ legacy option-2 offset) from lease env.
pub(crate) fn parse_dhcp_env(env: &BTreeMap<String, String>) -> Value {
    let mut out = Map::new();
    let tcode = env.get("opt101").or_else(|| env.get("tcode")).and_then(|v| decode_dhcp_string(v));
    let pcode = env.get("opt100").or_else(|| env.get("pcode")).and_then(|v| decode_dhcp_string(v));
    if let Some(tcode) = tcode {
        out.insert("tcode".into(), json!(tcode)); // IANA name
    }
    if let Some(pcode) = pcode {
        if let Some(offset) = parse_posix_tz_offset(&pcode) {
            out.insert("utc_offset_s".into(), json!(offset));
        }
        out.insert("pcode".into(), json!(pcode)); // POSIX TZ string
    }
    if let Some(tz) = env.get("timezone") {
        // busybox name for option 2: seconds EAST of UTC
        if let Ok(offset) = tz.parse::<i64>() {
            out.insert("utc_offset_s".into(), json!(offset));
        }
    }
    Value::Object(out)
}

/// Seconds east of UTC from a POSIX TZ string (sign is inverted: "CST-8"
/// means UTC+8 = +28800).
pub(crate) fn parse_posix_tz_offset(pcode: &str) -> Option<i64> {
    let bytes = pcode.as_bytes();
    let mut i = 0;
    // Skip the leading zone abbreviation (letters, or <...> quoted form).
    if bytes.first() == Some(&b'<') {
        while i < bytes.len() && bytes[i] != b'>' {
            i += 1;
        }
        i += 1;
    } else {
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
    }
    if i == 0 || i >= bytes.len() {
        return None;
    }
    let rest = &pcode[i..];
    let mut chars = rest.chars().peekable();
    let sign = match chars.peek() {
        Some('-') => {
            chars.next();
            -1
        }
        Some('+') => {
            chars.next();
            1
        }
        _ => 1,
    };
    let mut digits = String::new();
    while let Some(c) = chars.peek() {
        if c.is_ascii_digit() {
            digits.push(*c);
            chars.next();
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return None;
    }
    let hours: i64 = digits.parse().ok()?;
    let minutes = if chars.next() == Some(':') {
        let mut m = String::new();
        while let Some(c) = chars.peek() {
            if c.is_ascii_digit() {
                m.push(*c);
                chars.next();
            } else {
                break;
            }
        }
        m.parse().unwrap_or(0)
    } else {
        0
    };
    // POSIX: the value is what you ADD to local time to get UTC.
    Some(-sign * (hours * 3600 + minutes * 60))
}

fn is_loopback(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "127.0.1.1" | "localhost" | "::1" | "[::1]")
}

fn dhcp_iface_guess() -> Option<String> {
    let text = std::fs::read_to_string("/userdata/config/rknetd.ini").unwrap_or_default();
    for (iface, section) in [("eth0", "ethernet"), ("wlan0", "wifi")] {
        if !text.is_empty() {
            let marker = format!("[network.{section}]");
            if let Some(start) = text.find(&marker) {
                let body = &text[start + marker.len()..];
                let end = body.find("\n[").unwrap_or(body.len());
                if body[..end]
                    .lines()
                    .map(|l| l.trim().replace(' ', ""))
                    .any(|l| l.eq_ignore_ascii_case("method=dhcp"))
                {
                    return Some(iface.to_string());
                }
            }
        } else if std::fs::read_to_string(format!("/sys/class/net/{iface}/carrier"))
            .map(|c| c.trim() == "1")
            .unwrap_or(false)
        {
            return Some(iface.to_string());
        }
    }
    None
}

fn vote_winner(votes: &BTreeMap<String, u32>) -> Option<String> {
    votes
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(k, _)| k.clone())
}

// MARK: probe_timezone

pub async fn probe_timezone(
    _client: &ApiClient, // probes never touch the device API; kept for symmetry
    device: &DeviceRecord,
    dhcp_probe: bool,
    geoip_url: Option<&str>,
) -> Result<Value> {
    let on_device = is_loopback(&device.host);
    let perspective = if on_device { "device" } else { "sdk-host" };
    let mut sources: Vec<Value> = Vec::new();
    let mut candidates: BTreeMap<String, Vec<String>> = BTreeMap::new();

    macro_rules! back {
        ($tz:expr, $src:expr) => {{
            let entry = candidates.entry($tz.to_string()).or_default();
            if !entry.iter().any(|s| s == $src) {
                entry.push($src.to_string());
            }
        }};
    }

    // 1) Plain-HTTP geo-IP: the richest single source (IANA name directly).
    let url = geoip_url.unwrap_or(DEFAULT_GEOIP_URL);
    let mut geo_cc: Option<String> = None;
    match geoip(url, PROBE_TIMEOUT).await {
        Ok(geo) => {
            let tz = geo["timezone"].as_str().unwrap_or_default().to_string();
            geo_cc = geo
                .get("country_code")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut entry = json!({
                "source": "geoip",
                "status": "ok",
                "timezone": tz,
                "country_code": geo_cc,
                "endpoint": geo.get("url"),
            });
            if let Some(off) = utc_offset_seconds(&tz) {
                entry["utc_offset_s"] = json!(off);
            }
            sources.push(entry);
            back!(tz, "geoip");
        }
        Err(e) => sources.push(json!({
            "source": "geoip", "status": "unavailable", "reason": e.to_string(),
        })),
    }

    // 2) WiFi country (802.11d), on-device only.
    if on_device {
        match wifi_countries().await {
            Ok(votes) if !votes.is_empty() => {
                let winner = vote_winner(&votes);
                let zones = winner.as_deref().and_then(timezones_for_country);
                let conflicts = match (&geo_cc, &winner) {
                    (Some(cc), Some(w)) => cc != w,
                    _ => false,
                };
                let mut entry = json!({
                    "source": "wifi_country",
                    "status": "ok",
                    "country_code": winner,
                    "votes": votes,
                });
                if conflicts {
                    // e.g. a VPN egress abroad vs local RF — never auto-resolve.
                    entry["conflicts_with_geoip"] = json!(true);
                }
                match zones {
                    Some(zones) => {
                        entry["timezones"] = json!(zones);
                        entry["ambiguous"] = json!(zones.len() > 1);
                        if zones.len() == 1 {
                            back!(zones[0], "wifi_country");
                        }
                    }
                    None => {
                        entry["timezones"] = Value::Null;
                        entry["ambiguous"] = json!(true);
                    }
                }
                sources.push(entry);
            }
            Ok(_) => sources.push(json!({
                "source": "wifi_country", "status": "unavailable",
                "reason": "no WiFi country IEs seen (radio off, no APs, or iw missing)",
            })),
            Err(e) => sources.push(json!({
                "source": "wifi_country", "status": "unavailable", "reason": e.to_string(),
            })),
        }
    } else {
        sources.push(json!({
            "source": "wifi_country", "status": "skipped",
            "reason": "WiFi beacons can only be scanned on the camera itself",
        }));
    }

    // 3) DHCP timezone options (opt-in, on-device only).
    if !on_device {
        sources.push(json!({
            "source": "dhcp", "status": "skipped",
            "reason": "DHCP leases can only be probed on the camera itself",
        }));
    } else if !dhcp_probe {
        sources.push(json!({
            "source": "dhcp", "status": "skipped",
            "reason": "passive capture unavailable on stock firmware; re-run with dhcp_probe=true for a one-shot lease probe",
        }));
    } else {
        match dhcp_iface_guess() {
            None => sources.push(json!({
                "source": "dhcp", "status": "skipped",
                "reason": "no DHCP-managed interface found (static addressing)",
            })),
            Some(iface) => match dhcp_tz_probe(&iface, Duration::from_secs(12)).await {
                Ok(lease) if lease.as_object().map(|o| !o.is_empty()).unwrap_or(false) => {
                    if let Some(tcode) = lease.get("tcode").and_then(|v| v.as_str()) {
                        back!(tcode, "dhcp");
                    }
                    let mut entry = json!({"source": "dhcp", "status": "ok", "interface": iface});
                    entry.as_object_mut().unwrap().extend(
                        lease.as_object().unwrap().clone(),
                    );
                    sources.push(entry);
                }
                Ok(_) => sources.push(json!({
                    "source": "dhcp", "status": "unavailable", "interface": iface,
                    "reason": "lease carried no timezone options (100/101/2)",
                })),
                Err(e) => sources.push(json!({
                    "source": "dhcp", "status": "unavailable", "interface": iface,
                    "reason": e.to_string(),
                })),
            },
        }
    }

    // Aggregate.
    let mut notes: Vec<String> = Vec::new();
    let mut suggested: Option<String> = None;
    let mut needs_user = false;
    let geo_ok = sources.iter().find(|s| {
        s.get("source").and_then(|v| v.as_str()) == Some("geoip")
            && s.get("status").and_then(|v| v.as_str()) == Some("ok")
    });
    let wifi_ok = sources.iter().find(|s| {
        s.get("source").and_then(|v| v.as_str()) == Some("wifi_country")
            && s.get("status").and_then(|v| v.as_str()) == Some("ok")
    });
    if let (Some(g), Some(w)) = (geo_ok, wifi_ok) {
        let gcc = g.get("country_code").and_then(|v| v.as_str());
        let wcc = w.get("country_code").and_then(|v| v.as_str());
        if let (Some(gcc), Some(wcc)) = (gcc, wcc) {
            if gcc != wcc {
                needs_user = true;
                notes.push(format!(
                    "geo-IP says {gcc} but WiFi beacons say {wcc} (VPN egress or \
                     misconfigured APs?) — confirm with the user"
                ));
            }
        }
    }
    let mut ranked: Vec<(&String, &Vec<String>)> = candidates.iter().collect();
    ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
    if let Some((tz, _)) = ranked.first() {
        suggested = Some((*tz).clone());
        if ranked.len() > 1 {
            needs_user = true;
            let detail = candidates
                .iter()
                .map(|(tz, srcs)| format!("{tz} (via {})", srcs.join(", ")))
                .collect::<Vec<_>>()
                .join("; ");
            notes.push(format!("sources disagree: {detail}"));
        }
    } else {
        let wifi = sources.iter().find(|s| {
            s.get("source").and_then(|v| v.as_str()) == Some("wifi_country")
                && s.get("status").and_then(|v| v.as_str()) == Some("ok")
        });
        if let Some(wifi) = wifi {
            if let Some(zones) = wifi.get("timezones").and_then(|v| v.as_array()) {
                if zones.len() == 1 {
                    suggested = zones[0].as_str().map(String::from);
                } else if !zones.is_empty() {
                    needs_user = true;
                    let cc = wifi.get("country_code").and_then(|v| v.as_str()).unwrap_or("?");
                    let list = zones
                        .iter()
                        .filter_map(|z| z.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    notes.push(format!("country {cc} spans several timezones: {list}"));
                }
            }
        }
    }
    if !on_device {
        if suggested.is_some() {
            needs_user = true;
        }
        notes.push(
            "probes ran on the SDK host, not the camera — confirm the camera shares this network location before applying".into(),
        );
    }
    if suggested.is_none() && !needs_user {
        notes.push("no timezone evidence available; ask the user for their timezone".into());
    }

    Ok(json!({
        "perspective": perspective,
        "suggested_timezone": suggested,
        "needs_user_confirmation": needs_user,
        "candidates": candidates.iter().map(|(tz, srcs)| json!({"timezone": tz, "via": srcs})).collect::<Vec<_>>(),
        "sources": sources,
        "notes": notes,
    }))
}

// MARK: Reference clock (never the broken local clock)

/// Resolve a trustworthy "now" without trusting a possibly-broken clock:
/// explicit parameter → one SNTP query → HTTP Date headers → the SDK host
/// clock (valid only when the MCP runs on a *different* machine).
async fn reference_time(
    explicit: Option<i64>,
    ntp_server: Option<&str>,
    ntp_port: u16,
    on_device: bool,
) -> (Option<f64>, Value) {
    if let Some(ts) = explicit {
        return (
            Some(ts as f64),
            json!({"source": "parameter", "detail": "caller-provided reference_unix"}),
        );
    }
    let server = ntp_server.filter(|s| !s.trim().is_empty()).unwrap_or("pool.ntp.org");
    let sntp_err = match tokio::time::timeout(
        PROBE_TIMEOUT + Duration::from_secs(2),
        sntp_query(server, ntp_port, PROBE_TIMEOUT),
    )
    .await
    {
        Ok(Ok((ts, rtt))) => {
            return (
                Some(ts),
                json!({"source": "sntp", "detail": format!("{server}:{ntp_port}"),
                       "rtt_s": (rtt * 1000.0).round() / 1000.0}),
            );
        }
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some("timed out".to_string()),
    };
    reference_time_http_date(sntp_err, on_device).await
}

async fn reference_time_http_date(
    sntp_err: Option<String>,
    on_device: bool,
) -> (Option<f64>, Value) {
    let mut dates: Vec<(f64, f64, &str)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for url in DEFAULT_HTTP_DATE_URLS {
        match http_date(url, PROBE_TIMEOUT).await {
            Ok((ts, rtt)) => dates.push((ts, rtt, url)),
            Err(e) => errors.push(format!("{url}: {e}")),
        }
        if dates.len() >= 2 {
            break;
        }
    }
    if !dates.is_empty() {
        dates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let best = dates[0];
        if dates.len() > 1 && (dates[0].0 - dates[1].0).abs() > 10.0 {
            errors.push("HTTP Date sources disagree by >10s".into());
        }
        let mut ref_info = json!({"source": "http_date", "detail": best.2,
                                  "rtt_s": (best.1 * 1000.0).round() / 1000.0});
        if !errors.is_empty() {
            ref_info["warnings"] = json!(errors);
        }
        return (Some(best.0), ref_info);
    }
    if !on_device {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        return (
            Some(now),
            json!({
                "source": "sdk_host_clock",
                "detail": "MCP runs on another machine; its clock is independent of the camera",
                "ntp_error": sntp_err,
                "http_date_errors": errors,
            }),
        );
    }
    (
        None,
        json!({
            "source": "none",
            "detail": "running on the camera, so the local clock is not a valid reference",
            "ntp_error": sntp_err,
            "http_date_errors": errors,
        }),
    )
}

// MARK: sync_system_time

#[allow(clippy::too_many_arguments)]
pub async fn sync_system_time(
    client: &ApiClient,
    device: &DeviceRecord,
    timezone: Option<&str>, // None = keep, Some("auto") = probe, Some(name) = apply
    method: &str,
    ntp_server: Option<&str>,
    tolerance_s: i64,
    wait_s: u64,
    reference_unix: Option<i64>,
    dhcp_probe: bool,
    dry_run: bool,
) -> Result<Value> {
    let on_device = is_loopback(&device.host);
    let method = method.trim().to_ascii_lowercase();
    anyhow::ensure!(
        matches!(method.as_str(), "auto" | "ntp" | "manual"),
        "Invalid method {method:?}: expected auto|ntp|manual."
    );

    let before = get_system_time(client, device).await?;
    let before_ts = before
        .get("timestamp")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cur_ntp = before.get("ntp").cloned().unwrap_or(json!({}));
    let cur_server = cur_ntp
        .get("address")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let server = ntp_server
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| if cur_server.is_empty() { "pool.ntp.org".into() } else { cur_server.clone() });
    let port: u16 = cur_ntp
        .get("port")
        .and_then(|v| v.as_str())
        .and_then(|p| p.parse().ok())
        .unwrap_or(123);

    let (ref_ts, ref_info) =
        reference_time(reference_unix, Some(&server), port, on_device).await;
    let mut result = json!({
        "changed": false,
        "verified": false,
        "method_used": Value::Null,
        "reference": ref_info,
        "timezone": {"before": before.get("timezone"), "requested": timezone,
                     "applied": Value::Null, "verified": Value::Null},
        "warnings": [],
        "follow_up": [],
    });
    let Some(ref_ts) = ref_ts else {
        result["needs_user_help"] = json!(true);
        result["ask_user"] = json!(
            "I cannot determine the real time from here: NTP is unreachable and no \
             plain-HTTP time source answered. Please either check the network (DNS, \
             firewall blocking UDP 123 / HTTP) or tell me the current time so I can \
             set it manually."
        );
        result["drift_before_s"] = Value::Null;
        return Ok(result);
    };

    let mono0 = Instant::now();
    let ref_now = |at: Instant| -> f64 { ref_ts + (at - mono0).as_secs_f64() };
    let drift_before = before_ts as f64 - ref_ts;
    result["drift_before_s"] = json!((drift_before * 100.0).round() / 100.0);
    let need_clock = drift_before.abs() > tolerance_s as f64;

    // --- timezone decision (independent from the clock fix) ---
    let mut tz_apply: Option<String> = None;
    match timezone {
        Some("auto") => {
            // A probing failure must never block the clock fix.
            let probe = match probe_timezone(client, device, dhcp_probe, None).await {
                Ok(p) => Some(p),
                Err(e) => {
                    push_str(&mut result, "warnings",
                        &format!("timezone probe failed (clock fix continues): {e}"));
                    None
                }
            };
            match probe {
                None => push_str(&mut result, "follow_up",
                    "timezone could not be probed — set it explicitly or ask the user"),
                Some(probe) => {
                    result["timezone"]["evidence"] = json!({
                        "suggested": probe["suggested_timezone"],
                        "needs_user_confirmation": probe["needs_user_confirmation"],
                        "candidates": probe["candidates"],
                        "perspective": probe["perspective"],
                    });
                    let suggested = probe["suggested_timezone"].as_str().map(String::from);
                    let probe_needs_user =
                        probe["needs_user_confirmation"].as_bool().unwrap_or(false);
                    let before_tz =
                        before.get("timezone").and_then(|v| v.as_str()).unwrap_or("");
                    match suggested {
                        Some(tz) if !probe_needs_user => tz_apply = Some(tz),
                        Some(tz) if tz == before_tz => {
                            // Evidence agrees with the current timezone; nothing to ask.
                        }
                        Some(_tz) => {
                            result["timezone"]["needs_user_confirmation"] = json!(true);
                            let cand = probe["candidates"]
                                .as_array()
                                .map(|c| {
                                    c.iter()
                                        .filter_map(|e| e.get("timezone").and_then(|t| t.as_str()))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                                .unwrap_or_default();
                            if probe["perspective"].as_str() != Some("device") {
                                push_str(&mut result, "follow_up", &format!(
                                    "timezone evidence ({cand}) comes from the SDK host's \
                                     network, not the camera's — confirm with the user \
                                     before applying"));
                            } else {
                                push_str(&mut result, "follow_up", &format!(
                                    "timezone evidence is ambiguous — ask the user; \
                                     candidates: {cand}"));
                            }
                        }
                        None => push_str(&mut result, "follow_up",
                            "timezone could not be determined automatically — ask the user"),
                    }
                }
            }
        }
        Some(tz) => tz_apply = Some(tz.to_string()),
        None => {}
    }

    let before_tz = before.get("timezone").and_then(|v| v.as_str()).unwrap_or("");
    let mut need_tz = tz_apply.as_deref().map(|t| t != before_tz).unwrap_or(false);
    if need_tz {
        if let (Some(tz), false) = (tz_apply.as_deref(), before_tz.is_empty()) {
            if let (Some(a), Some(b)) = (utc_offset_seconds(tz), utc_offset_seconds(before_tz)) {
                if a == b {
                    // tzdb-equivalent alias: switching would be churn, not a correction.
                    push_str(&mut result, "warnings", &format!(
                        "timezone {before_tz:?} is behaviorally identical to {tz:?}; keeping it"));
                    tz_apply = None;
                    need_tz = false;
                }
            }
        }
    }

    let cur_method = before
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let want_ntp = method == "ntp"
        || (method == "auto" && ref_info.get("source").and_then(|v| v.as_str()) == Some("sntp"));
    // An explicit ntp request (or a server override) must reconfigure even when
    // the clock is currently fine; auto also upgrades a pinned-manual device
    // back to self-healing NTP when a server is reachable.
    let need_method = want_ntp && (cur_method != "ntp" || (ntp_server.is_some() && server != cur_server));

    if !need_clock && !need_tz && !need_method {
        result["verified"] = json!(true);
        result["method_used"] = before.get("method").cloned().unwrap_or(Value::Null);
        result["timezone"]["verified"] = json!(true);
        result["drift_after_s"] = json!((drift_before * 100.0).round() / 100.0);
        result["ntp"] = before.get("ntp").cloned().unwrap_or(Value::Null);
        return Ok(result);
    }

    if dry_run {
        let would = if !need_clock && !need_method {
            if cur_method == "ntp" || cur_method == "manual" {
                cur_method.clone()
            } else {
                "ntp".to_string()
            }
        } else if want_ntp {
            "ntp".to_string()
        } else if method == "auto" {
            "manual".to_string()
        } else {
            method.clone()
        };
        result["dry_run"] = json!({
            "would_set_method": would,
            "would_set_timezone": if need_tz { tz_apply.clone() } else { None },
            "would_set_timestamp": if need_clock { json!(ref_ts as i64) } else { Value::Null },
        });
        return Ok(result);
    }

    // --- apply ---
    let tz_only = !need_clock && !need_method;
    let method_used = if tz_only {
        // Timezone-only change: keep the current sync mode (no ntp->manual
        // churn); a manual PUT reuses the device's own timestamp so the clock
        // is not touched at all.
        if cur_method == "ntp" || cur_method == "manual" {
            cur_method.as_str().to_string()
        } else {
            "ntp".to_string()
        }
    } else if want_ntp {
        "ntp".to_string()
    } else if method == "auto" {
        "manual".to_string()
    } else {
        method.clone()
    };
    result["method_used"] = json!(method_used);

    let mut after;
    {
        let apply = async {
            if tz_only {
                set_system_time(
                    client,
                    device,
                    SetSystemTimeArgs {
                        method: method_used.clone(),
                        timezone: tz_apply.clone(),
                        timestamp: if method_used == "manual" { Some(before_ts) } else { None },
                        ntp_server: if method_used == "ntp" { Some(server.clone()) } else { None },
                        ntp_port: None,
                    },
                )
                .await?;
                return get_system_time(client, device).await;
            }
            if method_used == "ntp" {
                set_system_time(
                    client,
                    device,
                    SetSystemTimeArgs {
                        method: "ntp".into(),
                        timezone: if need_tz { tz_apply.clone() } else { None },
                        timestamp: None,
                        ntp_server: Some(server.clone()),
                        ntp_port: None,
                    },
                )
                .await?;
                // The PUT triggers an immediate sync on the device; poll for it.
                let deadline = Instant::now() + Duration::from_secs(wait_s.max(1));
                let mut cur = get_system_time(client, device).await?;
                while Instant::now() < deadline {
                    let drift = cur
                        .get("timestamp")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as f64
                        - ref_now(Instant::now());
                    if drift.abs() <= tolerance_s as f64 {
                        return Ok(cur);
                    }
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    cur = get_system_time(client, device).await?;
                }
                Ok(cur)
            } else {
                set_system_time(
                    client,
                    device,
                    SetSystemTimeArgs {
                        method: "manual".into(),
                        timezone: if need_tz { tz_apply.clone() } else { None },
                        timestamp: Some(ref_ts as i64),
                        ntp_server: None,
                        ntp_port: None,
                    },
                )
                .await?;
                get_system_time(client, device).await
            }
        };
        match apply.await {
            Ok(cur) => after = cur,
            Err(e) => {
                // A timezone rejection raises *after* the clock part was
                // applied; surface it, keep verifying, don't mask partial success.
                push_str(&mut result, "warnings", &e.to_string());
                after = get_system_time(client, device).await?;
            }
        }
    }

    let drift_after = after
        .get("timestamp")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        - ref_now(Instant::now());
    result["drift_after_s"] = json!((drift_after * 100.0).round() / 100.0);
    result["changed"] = json!(true);
    result["verified"] = json!(drift_after.abs() <= tolerance_s as f64);
    result["timezone"]["applied"] = after.get("timezone").cloned().unwrap_or(Value::Null);
    result["timezone"]["verified"] = json!(
        tz_apply.is_none() || after.get("timezone").and_then(|v| v.as_str()) == tz_apply.as_deref()
    );
    result["ntp"] = after.get("ntp").cloned().unwrap_or(Value::Null);

    if method_used == "manual" && method == "auto" && cur_method == "ntp" {
        // The auto-mode stopgap PUT overwrote the sync mode; hand NTP back so
        // the device keeps correcting itself once the server is reachable.
        // (An explicit method=manual is the user's choice — leave it alone.)
        match set_system_time(
            client,
            device,
            SetSystemTimeArgs {
                method: "ntp".into(),
                timezone: None,
                timestamp: None,
                ntp_server: Some(server.clone()),
                ntp_port: None,
            },
        )
        .await
        {
            Ok(restored) => {
                result["method_restored"] = json!("ntp");
                result["ntp"] = restored.get("ntp").cloned().unwrap_or(Value::Null);
                after["method"] = json!("ntp");
            }
            Err(e) => push_str(&mut result, "warnings",
                &format!("could not restore NTP mode after manual stopgap: {e}")),
        }
    }

    if method_used == "ntp" && result["verified"] != json!(true) {
        push_str(&mut result, "warnings", &format!(
            "NTP did not sync within {wait_s}s (server {server}:{port}). The config is \
             stored and the device keeps retrying hourly; re-run with method=manual for \
             an immediate stopgap."));
        push_str(&mut result, "follow_up",
            "check DNS and that outbound UDP 123 is allowed; consider a local NTP server");
    }
    if method_used == "manual" {
        push_str(&mut result, "warnings", &format!(
            "clock was set manually from {}; it will drift without NTP. Re-run \
             sync_system_time once NTP works.",
            ref_info.get("source").and_then(|v| v.as_str()).unwrap_or("a reference")));
    }
    if result["verified"] == json!(true)
        && result["timezone"].get("needs_user_confirmation") != Some(&json!(true))
        && result["follow_up"].as_array().map(|a| a.is_empty()).unwrap_or(true)
    {
        result["follow_up"] = json!(["none — clock verified"]);
    }
    Ok(result)
}

fn push_str(result: &mut Value, key: &str, msg: &str) {
    if let Some(arr) = result.get_mut(key).and_then(|v| v.as_array_mut()) {
        arr.push(json!(msg));
    }
}

// MARK: Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_date_parses_imf_fixdate() {
        let ts = parse_http_date("Fri, 13 Nov 2026 14:22:46 GMT").unwrap();
        assert_eq!(ts as i64, 1_794_579_766);
        assert!(parse_http_date("garbage").is_none());
        assert!(parse_http_date("Fri, 32 Nov 2026 99:99:99 GMT").is_none());
    }

    #[test]
    fn days_from_civil_epoch_anchor() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
    }

    #[test]
    fn tz_offset_parsing() {
        assert_eq!(parse_tz_offset("+0800"), Some(28_800));
        assert_eq!(parse_tz_offset("-0530"), Some(-19_800));
        assert_eq!(parse_tz_offset("+0000"), Some(0));
        assert_eq!(parse_tz_offset("UTC"), None);
    }

    #[test]
    fn posix_offset_sign_is_inverted() {
        assert_eq!(parse_posix_tz_offset("CST-8"), Some(28_800));
        assert_eq!(parse_posix_tz_offset("EST5"), Some(-18_000));
        assert_eq!(parse_posix_tz_offset("UTC0"), Some(0));
        assert_eq!(parse_posix_tz_offset("garbage!!"), None);
    }

    #[test]
    fn dhcp_env_parses_rfc4833_hex() {
        let env: BTreeMap<String, String> = [
            ("opt100".to_string(), "4353542d38".to_string()),
            ("opt101".to_string(), "417369612f5368616e67686169".to_string()),
        ]
        .into_iter()
        .collect();
        let out = parse_dhcp_env(&env);
        assert_eq!(out["pcode"], json!("CST-8"));
        assert_eq!(out["tcode"], json!("Asia/Shanghai"));
        assert_eq!(out["utc_offset_s"], json!(28_800));
    }

    #[test]
    fn dhcp_env_option2_and_raw_strings() {
        let env: BTreeMap<String, String> = [
            ("timezone".to_string(), "-18000".to_string()),
            ("opt101".to_string(), "Europe/Berlin".to_string()),
        ]
        .into_iter()
        .collect();
        let out = parse_dhcp_env(&env);
        assert_eq!(out["tcode"], json!("Europe/Berlin"));
        assert_eq!(out["utc_offset_s"], json!(-18_000));
    }

    #[test]
    fn timezone_name_validation() {
        assert!(valid_timezone_name("Asia/Shanghai"));
        assert!(valid_timezone_name("America/Argentina/Buenos_Aires"));
        assert!(!valid_timezone_name("bad zone"));
        assert!(!valid_timezone_name("../etc/passwd"));
        assert!(!valid_timezone_name(""));
    }

    #[test]
    fn vote_winner_breaks_ties_deterministically() {
        let mut votes = BTreeMap::new();
        votes.insert("US".to_string(), 3);
        votes.insert("CN".to_string(), 40);
        assert_eq!(vote_winner(&votes), Some("CN".into()));
    }
}
