//! Path and URL filters for the built-in local tools. Default-deny on
//! known-sensitive locations (ssh/aws/gnupg keys, our own .env, private
//! network ranges) so a prompt-injected tool call can't trivially read
//! credentials or talk to internal services. User config can extend
//! either list; the allow list wins over the deny list.

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Built-in deny prefixes for `read_file` / `list_dir`. Matched against
/// the *canonicalized* requested path so a symlink can't bypass them.
const DEFAULT_DENY_PATHS: &[&str] = &[
    "~/.ssh/",
    "~/.aws/",
    "~/.gnupg/",
    "~/.netrc",
];

/// Built-in deny patterns for `fetch_url` host strings. Lower-cased
/// prefix match against the URL's host component. IPv4 RFC1918 / CGNAT /
/// link-local ranges are handled algorithmically in `is_private_ipv4`.
const DEFAULT_DENY_HOSTS: &[&str] = &[
    "localhost",
    "127.",
    "::1",
    "0.",
];

struct Rules {
    read_deny: Vec<PathBuf>,
    read_allow: Vec<PathBuf>,
    fetch_deny: Vec<String>,
    fetch_allow: Vec<String>,
}

static RULES: OnceLock<Rules> = OnceLock::new();

fn rules() -> &'static Rules {
    RULES.get_or_init(load_rules)
}

fn load_rules() -> Rules {
    let cfg = crate::config::load().unwrap_or_default();
    // Also deny our own .env (lives in <config_dir>) so a tool call can't
    // exfiltrate the API key by reading it back as a file.
    let mut deny_str: Vec<String> = DEFAULT_DENY_PATHS.iter().map(|s| s.to_string()).collect();
    if let Ok(paths) = crate::config::paths()
        && let Some(p) = paths.config_dir.join(".env").to_str()
    {
        deny_str.push(p.to_string());
    }
    deny_str.extend(cfg.security.read_paths_deny.iter().cloned());

    Rules {
        read_deny: deny_str
            .iter()
            .map(|s| PathBuf::from(crate::output::expand_path(s)))
            .collect(),
        read_allow: cfg
            .security
            .read_paths_allow
            .iter()
            .map(|s| PathBuf::from(crate::output::expand_path(s)))
            .collect(),
        fetch_deny: DEFAULT_DENY_HOSTS
            .iter()
            .map(|s| s.to_lowercase())
            .chain(cfg.security.fetch_hosts_deny.iter().map(|s| s.to_lowercase()))
            .collect(),
        fetch_allow: cfg
            .security
            .fetch_hosts_allow
            .iter()
            .map(|s| s.to_lowercase())
            .collect(),
    }
}

/// Verify that the requested path is safe to read. Returns the canonical
/// resolved path on success, or an error explaining the denial.
pub fn check_read_path(input: &str) -> Result<PathBuf> {
    let expanded = crate::output::expand_path(input);
    let canonical = std::fs::canonicalize(&expanded)
        .map_err(|e| anyhow::anyhow!("cannot resolve {expanded}: {e}"))?;
    let r = rules();
    // Allow wins.
    if r.read_allow.iter().any(|p| path_under(&canonical, p)) {
        return Ok(canonical);
    }
    if let Some(matched) = r.read_deny.iter().find(|p| path_under(&canonical, p)) {
        bail!(
            "path '{}' is in the security deny list (matched '{}'). \
             Configure [security].read_paths_allow to permit it explicitly.",
            canonical.display(),
            matched.display()
        );
    }
    Ok(canonical)
}

/// Verify that the host of `url` is safe to fetch. Returns Ok on success
/// or an error explaining the denial.
pub fn check_fetch_url(url: &str) -> Result<()> {
    let host = url_host(url).unwrap_or_default().to_lowercase();
    if host.is_empty() {
        bail!("could not extract host from URL '{url}'");
    }
    let r = rules();
    if r.fetch_allow.iter().any(|allow| host_matches(&host, allow)) {
        return Ok(());
    }
    if is_private_ipv4(&host) {
        bail!(
            "host '{host}' resolves to a private/link-local IPv4 range. \
             Configure [security].fetch_hosts_allow to permit it."
        );
    }
    if let Some(matched) = r.fetch_deny.iter().find(|deny| host_matches(&host, deny)) {
        bail!(
            "host '{host}' is in the security deny list (matched '{matched}'). \
             Configure [security].fetch_hosts_allow to permit it."
        );
    }
    Ok(())
}

fn path_under(candidate: &std::path::Path, ancestor: &std::path::Path) -> bool {
    // Equal-path also counts as "under" (denying a file matches that file).
    candidate == ancestor || candidate.starts_with(ancestor)
}

fn host_matches(host: &str, pattern: &str) -> bool {
    // Exact match, or prefix-with-dot (so "127." matches "127.0.0.1"), or
    // suffix-with-dot ("internal.example" matches "x.internal.example").
    host == pattern
        || host.starts_with(pattern)
        || (pattern.starts_with('.') && host.ends_with(pattern))
}

fn url_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    // Strip path/query/fragment.
    let host_port = rest.split(['/', '?', '#']).next()?;
    // Strip user-info (after the rightmost '@', if any).
    let host_port = host_port.rsplit_once('@').map(|(_, h)| h).unwrap_or(host_port);
    // IPv6 literals are bracketed: [::1]:8080 → ::1.
    if let Some(stripped) = host_port.strip_prefix('[') {
        return stripped.split(']').next();
    }
    // Strip the port (everything after the first ':').
    Some(host_port.split_once(':').map(|(h, _)| h).unwrap_or(host_port))
}

fn is_private_ipv4(addr: &str) -> bool {
    // Quick reject: must be four dotted octets of digits.
    let octets: Vec<&str> = addr.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    let parsed: Option<Vec<u8>> = octets.iter().map(|s| s.parse::<u8>().ok()).collect();
    let Some(o) = parsed else { return false };
    match o.as_slice() {
        [127, _, _, _] => true,                 // loopback
        [10, _, _, _] => true,                  // RFC1918
        [192, 168, _, _] => true,               // RFC1918
        [172, b, _, _] if (16..=31).contains(b) => true, // RFC1918
        [169, 254, _, _] => true,               // link-local incl. cloud metadata
        [0, _, _, _] => true,                   // 0.0.0.0/8
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ipv4_detection() {
        assert!(is_private_ipv4("127.0.0.1"));
        assert!(is_private_ipv4("10.0.0.5"));
        assert!(is_private_ipv4("192.168.1.1"));
        assert!(is_private_ipv4("172.16.0.1"));
        assert!(is_private_ipv4("172.31.255.254"));
        assert!(is_private_ipv4("169.254.169.254"));
        assert!(!is_private_ipv4("172.15.0.1"));
        assert!(!is_private_ipv4("172.32.0.1"));
        assert!(!is_private_ipv4("8.8.8.8"));
        assert!(!is_private_ipv4("example.com"));
    }

    #[test]
    fn host_extraction() {
        assert_eq!(url_host("http://example.com/x"), Some("example.com"));
        assert_eq!(url_host("https://example.com:8443/x?q=1"), Some("example.com"));
        assert_eq!(url_host("https://user:pw@host.example/"), Some("host.example"));
        assert_eq!(url_host("http://[::1]:8080/"), Some("::1"));
        assert_eq!(url_host("ftp://example.com/"), None);
    }

    #[test]
    fn host_match_variants() {
        assert!(host_matches("127.0.0.1", "127."));
        assert!(host_matches("localhost", "localhost"));
        assert!(host_matches("api.internal", ".internal"));
        assert!(!host_matches("example.com", "internal"));
    }
}
