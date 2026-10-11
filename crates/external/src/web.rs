//! Web addresses the browser may be sent to for Create ▸ From web page.

use std::net::{Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

use crate::ExternalError;

/// The longest address accepted.
const MAX_URL: usize = 2048;

fn hidden(c: char) -> bool {
    c.is_control() || c.is_whitespace() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

/// A web address safe to hand to the browser: `http` or `https` with a host, no user name or
/// password, no spaces or invisible characters, and not this computer, a private network or a
/// local name. Numeric addresses are checked in every spelling the URL standard accepts
/// (`2130706433`, `0x7f.1`, IPv4-mapped IPv6). A public name that resolves to a private address is
/// not caught here: the browser resolves names itself.
pub fn checked_web_url(input: &str) -> Result<Url, ExternalError> {
    let shown: String = input.chars().filter(|c| !hidden(*c)).take(80).collect();
    let bad = |why: &str| ExternalError::InvalidInput(format!("{why}: {shown}"));
    let t = input.trim_matches(|c: char| c == ' ');
    if t.is_empty() {
        return Err(bad("no web address given"));
    }
    if t.len() > MAX_URL {
        return Err(bad("the web address is too long"));
    }
    if t.chars().any(hidden) {
        return Err(bad("the web address contains spaces or invisible characters"));
    }
    let url = Url::parse(t).map_err(|_| bad("not a web address"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad("only http and https addresses can be converted"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("addresses with a user name or password are not opened"));
    }
    let public = match url.host() {
        None => false,
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d.contains('.') && !["localhost", ".localhost", ".local", ".internal", ".lan", ".home.arpa"].iter().any(|s| d == *s || d.ends_with(*s))
        }
        Some(Host::Ipv4(ip)) => public_v4(ip),
        Some(Host::Ipv6(ip)) => public_v6(ip),
    };
    if !public {
        return Err(bad("addresses on this computer or a private network are not opened"));
    }
    Ok(url)
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || a >= 240
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b)))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let [s0, s1, ..] = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (s0 & 0xfe00) == 0xfc00
        || (s0 & 0xffc0) == 0xfe80
        || (s0 == 0x2001 && s1 == 0x0db8)
        || (s0 == 0x0064 && s1 == 0xff9b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_web_addresses_pass() {
        for ok in [
            "https://example.org/a?b=c#d",
            "http://example.org:8080/x",
            "https://93.184.215.14/",
            "https://[2606:2800:21f:cb07:6820:80da:af6b:8b2c]/",
        ] {
            assert!(checked_web_url(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn local_private_and_odd_addresses_are_refused() {
        for bad in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "chrome://settings",
            "about:blank",
            "ftp://example.org/x",
            "http://localhost:8080/",
            "http://app.localhost/",
            "http://printer.local/",
            "http://intranet/",
            "http://127.0.0.1/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://10.1.2.3/",
            "http://192.168.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[::ffff:10.0.0.1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "https://user:pw@example.org/",
            "https://exa mple.org/",
            "https://example.org/\u{202e}gpj.exe",
            "--enable-automation",
        ] {
            assert!(matches!(checked_web_url(bad), Err(ExternalError::InvalidInput(_))), "{bad:?} must be refused");
        }
        assert!(checked_web_url(&format!("https://example.org/{}", "a".repeat(MAX_URL))).is_err());
    }
}
