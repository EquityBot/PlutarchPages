//! Asks GitHub for the latest PlutarchPages release (Help ▸ Check for updates, issue #28).

use std::time::Duration;

use pdfcraft_ui_egui::updates::{RELEASES_PAGE, Release};

const LATEST: &str = "https://api.github.com/repos/EquityBot/PlutarchPages/releases/latest";

/// What GitHub's 404 for [`LATEST`] means: the fork has not published a release yet.
const NO_RELEASE: &str = "PlutarchPages hasn't published a release yet; build it from source";

/// The latest release. The answer is untrusted: its size is capped, and only a page under
/// [`RELEASES_PAGE`] is ever offered for download (anything else falls back to that list).
pub fn latest_release() -> Result<Release, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(os_roots()?).build())
        .build()
        .new_agent();
    let mut response = agent
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", concat!("PlutarchPages/", env!("CARGO_PKG_VERSION")))
        // Read error statuses ourselves: a 403 from GitHub usually means its anonymous
        // rate limit ran out, and the retry time rides along in a response header.
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| format!("couldn't reach GitHub ({e})"))?;
    let status = response.status().as_u16();
    let reset = response.headers().get("x-ratelimit-reset").and_then(|v| v.to_str().ok());
    if let Some(e) = status_error(status, reset, now_unix()) {
        return Err(e);
    }
    let body = response.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| format!("unreadable answer ({e})"))?;
    parse(&body)
}

/// The certificate authorities the operating system trusts.
fn os_roots() -> Result<ureq::tls::RootCerts, String> {
    let found = rustls_native_certs::load_native_certs();
    let certs: Vec<ureq::tls::Certificate<'static>> = found.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
    if certs.is_empty() {
        return Err("no trusted certificates found on this system".into());
    }
    Ok(ureq::tls::RootCerts::new_with_certs(&certs))
}

/// The current Unix time in seconds (0 when the clock gives nothing usable).
fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What to tell the user when GitHub answers 403: its anonymous quota (60 checks an hour,
/// shared by everyone behind the same address) ran out. `reset` is the `x-ratelimit-reset`
/// header, the Unix time the quota refills; `now` is the current Unix time. Anything
/// surprising (no header, not a number, already past) falls back to asking for patience.
fn rate_limited(reset: Option<&str>, now: u64) -> String {
    const FALLBACK: &str = "GitHub is limiting update checks for now (shared hourly quota); try again later";
    let wait = reset.and_then(|r| r.trim().parse::<u64>().ok()).and_then(|reset| reset.checked_sub(now)).filter(|&wait| wait > 0);
    match wait {
        Some(wait) => {
            let minutes = wait.div_ceil(60).max(1);
            let plural = if minutes == 1 { "" } else { "s" };
            format!("GitHub is limiting update checks for now (shared hourly quota); try again in about {minutes} minute{plural}")
        }
        None => FALLBACK.into(),
    }
}

/// What to tell the user about a non-success HTTP status, or `None` for success. GitHub
/// answers 404 for `releases/latest` until the first release is published.
fn status_error(status: u16, reset: Option<&str>, now: u64) -> Option<String> {
    match status {
        200..300 => None,
        403 => Some(rate_limited(reset, now)),
        404 => Some(NO_RELEASE.into()),
        _ => Some(format!("couldn't reach GitHub (http status: {status})")),
    }
}

/// Whether `rest`, the part of a release URL after [`RELEASES_PAGE`], stays on that page once a
/// browser resolves it. Dot segments could climb to another repository's releases, so they are
/// refused, and so is `%`, which could encode one; anything refused falls back to the list.
fn offered_path(rest: &str) -> bool {
    rest.starts_with('/') && !rest.contains(['?', '#', '\\', '%']) && !rest.split('/').any(|segment| segment == "." || segment == "..")
}

fn parse(body: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("unreadable answer ({e})"))?;
    let version = v["tag_name"].as_str().filter(|t| !t.is_empty() && t.len() <= 64).ok_or("no release found")?.to_string();
    let url = v["html_url"].as_str().filter(|u| u.strip_prefix(RELEASES_PAGE).is_some_and(offered_path)).unwrap_or(RELEASES_PAGE).to_string();
    Ok(Release { version, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_and_only_our_release_pages_are_offered() {
        let r = parse(r#"{"tag_name":"v0.2.0","html_url":"https://github.com/EquityBot/PlutarchPages/releases/tag/v0.2.0"}"#).unwrap();
        assert_eq!(r, Release { version: "v0.2.0".into(), url: "https://github.com/EquityBot/PlutarchPages/releases/tag/v0.2.0".into() });
        // Upstream PdfCraft builds lack this fork's changes, so they are never offered either.
        for elsewhere in [
            "https://example.com/pdfcraft.exe",
            "https://github.com/EquityBot/PlutarchPages/releases.evil/x",
            "https://github.com/storytold/pdfcraft/releases/tag/v0.15.0",
            // Browsers resolve dot segments, so these would land on upstream's page.
            "https://github.com/EquityBot/PlutarchPages/releases/../../../storytold/pdfcraft/releases/tag/v0.15.0",
            "https://github.com/EquityBot/PlutarchPages/releases/tag/%2e%2e/%2e%2e/%2e%2e/%2e%2e/storytold/pdfcraft",
            "https://github.com/EquityBot/PlutarchPages/releases/./tag/v1",
            "javascript:alert(1)",
        ] {
            let r = parse(&format!(r#"{{"tag_name":"v9.9.9","html_url":"{elsewhere}"}}"#)).unwrap();
            assert_eq!(r.url, RELEASES_PAGE, "{elsewhere}");
        }
        assert!(parse(r#"{"message":"Not Found"}"#).is_err());
        assert!(parse("<html>").is_err());
    }

    #[test]
    fn rate_limit_messages_name_the_wait() {
        let limited = "GitHub is limiting update checks for now (shared hourly quota); try again in about 20 minutes";
        assert_eq!(rate_limited(Some("1800001200"), 1800000000), limited);
        // Rounds up: 61 seconds out reads as 2 minutes.
        assert_eq!(
            rate_limited(Some("1800000061"), 1800000000),
            "GitHub is limiting update checks for now (shared hourly quota); try again in about 2 minutes"
        );
        assert_eq!(
            rate_limited(Some("1800000060"), 1800000000),
            "GitHub is limiting update checks for now (shared hourly quota); try again in about 1 minute"
        );
        // No header, not a number, or already past: ask for patience instead of a time.
        let later = "GitHub is limiting update checks for now (shared hourly quota); try again later";
        for bad in [None, Some(""), Some("soon"), Some("1799999999"), Some("1800000000")] {
            assert_eq!(rate_limited(bad, 1800000000), later);
        }
    }

    #[test]
    fn statuses_read_as_messages() {
        assert_eq!(status_error(200, None, 0), None);
        assert_eq!(status_error(404, None, 0).as_deref(), Some(NO_RELEASE));
        assert_eq!(
            status_error(403, Some("1800001200"), 1800000000).as_deref(),
            Some("GitHub is limiting update checks for now (shared hourly quota); try again in about 20 minutes")
        );
        assert_eq!(status_error(500, None, 0).as_deref(), Some("couldn't reach GitHub (http status: 500)"));
    }

    /// Live: asks GitHub over TLS with the OS's roots (`cargo test -p pdfcraft -- --ignored`).
    #[test]
    #[ignore = "needs network access"]
    fn github_answers_with_the_latest_release() {
        match latest_release() {
            Ok(r) => {
                assert!(pdfcraft_ui_egui::updates::is_newer(&r.version, "0.0.0"), "{r:?}");
                assert!(r.url.starts_with(RELEASES_PAGE), "{r:?}");
            }
            // Until the fork's first release, GitHub answers 404.
            Err(e) => assert_eq!(e, NO_RELEASE),
        }
    }
}
