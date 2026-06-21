use std::net::IpAddr;
use std::str::FromStr;

use ipnet::IpNet;
use tianji_types::{ScopeRules, Target};

/// Is this target permitted by the engagement scope? Fail closed: an empty ruleset matches
/// nothing.
pub fn in_scope(target: &Target, scope: &ScopeRules) -> bool {
    match target {
        Target::Ip(ip) => scope.cidrs.iter().any(|c| ip_in_cidr(ip, c)),
        Target::Cidr(cidr) => scope.cidrs.iter().any(|c| cidr_in_cidr(cidr, c)),
        Target::Hostname(h) => scope
            .hostnames
            .iter()
            .any(|allowed| h == allowed || h.ends_with(&format!(".{allowed}")))
            // Also allow hostnames that match a bare-IP scope entry.
            || scope.cidrs.iter().any(|c| ip_in_cidr(h, c)),
        Target::Url(u) => {
            let host = url_host(u);
            if IpAddr::from_str(&host).is_ok() {
                // IP-addressed URL — check against CIDRs, same as Target::Ip.
                scope.cidrs.iter().any(|c| ip_in_cidr(&host, c))
            } else {
                // Hostname URL — check against hostnames and url_domains.
                scope.hostnames.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
                    || scope.url_domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
            }
        }
    }
}

/// Extract the host portion from a URL string (scheme://host:port/path → host).
fn url_host(url: &str) -> String {
    // Strip scheme.
    let rest = url.splitn(3, "//").nth(1).unwrap_or(url);
    // Take the authority portion (before the first '/').
    let authority = rest.split('/').next().unwrap_or(rest);
    // Handle IPv6 bracketed addresses like [::1]:8006.
    if authority.starts_with('[') {
        authority
            .split(']')
            .next()
            .and_then(|s| s.get(1..))
            .unwrap_or(authority)
            .to_string()
    } else {
        // Strip port.
        authority.split(':').next().unwrap_or(authority).to_string()
    }
}

/// Returns true iff `ip` is contained by `cidr` (exact CIDR containment).
fn ip_in_cidr(ip: &str, cidr: &str) -> bool {
    let Ok(addr) = IpAddr::from_str(ip) else { return false };
    match IpNet::from_str(cidr) {
        Ok(net) => net.contains(&addr),
        // Bare IP address in the scope list — treat as /32.
        Err(_) => IpAddr::from_str(cidr).map_or(false, |c| c == addr),
    }
}

/// Returns true iff `inner` is a subnet of (or equal to) `outer`.
fn cidr_in_cidr(inner: &str, outer: &str) -> bool {
    let (Ok(i), Ok(o)) = (IpNet::from_str(inner), IpNet::from_str(outer)) else {
        return false;
    };
    o.contains(&i.network()) && o.prefix_len() <= i.prefix_len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tianji_types::ScopeRules;

    fn scope(cidrs: &[&str]) -> ScopeRules {
        ScopeRules {
            cidrs: cidrs.iter().map(|s| s.to_string()).collect(),
            hostnames: vec![],
            url_domains: vec![],
        }
    }

    #[test]
    fn single_host_in_slash24() {
        assert!(in_scope(&Target::Ip("192.168.1.25".into()), &scope(&["192.168.1.0/24"])));
    }

    #[test]
    fn host_outside_slash24() {
        assert!(!in_scope(&Target::Ip("192.168.2.1".into()), &scope(&["192.168.1.0/24"])));
    }

    #[test]
    fn slash8_contains_arbitrary_host() {
        assert!(in_scope(&Target::Ip("10.42.7.99".into()), &scope(&["10.0.0.0/8"])));
    }

    #[test]
    fn slash16_boundary() {
        assert!(in_scope(&Target::Ip("172.16.0.1".into()), &scope(&["172.16.0.0/16"])));
        assert!(!in_scope(&Target::Ip("172.17.0.1".into()), &scope(&["172.16.0.0/16"])));
    }

    #[test]
    fn bare_ip_in_scope_list() {
        assert!(in_scope(&Target::Ip("192.168.1.25".into()), &scope(&["192.168.1.25"])));
        assert!(!in_scope(&Target::Ip("192.168.1.26".into()), &scope(&["192.168.1.25"])));
    }

    #[test]
    fn url_with_ip_host_checks_cidr() {
        let s = scope(&["192.168.1.0/24"]);
        assert!(in_scope(&Target::Url("https://192.168.1.25:8006/api2/json/version".into()), &s));
        assert!(in_scope(&Target::Url("http://192.168.1.25:3128/".into()), &s));
        assert!(!in_scope(&Target::Url("https://8.8.8.8/".into()), &s));
    }

    #[test]
    fn url_with_bare_ip_scope_entry() {
        let s = scope(&["192.168.1.25"]);
        assert!(in_scope(&Target::Url("https://192.168.1.25:8006/".into()), &s));
        assert!(!in_scope(&Target::Url("https://192.168.1.26:8006/".into()), &s));
    }

    #[test]
    fn url_with_hostname_checks_url_domains() {
        let s = ScopeRules {
            cidrs: vec![],
            hostnames: vec!["example.com".into()],
            url_domains: vec!["api.example.com".into()],
        };
        assert!(in_scope(&Target::Url("https://example.com/login".into()), &s));
        assert!(in_scope(&Target::Url("https://api.example.com/v1/".into()), &s));
        assert!(!in_scope(&Target::Url("https://other.com/".into()), &s));
    }
}
