//! Who may talk to the HTTP transport, decided once at startup.
//!
//! [`HttpPolicy`] is parsed from the config before the listener binds, so a
//! bind the operator did not opt into never happens. The per-request guards
//! (Host, Origin, bearer token) are pure functions of the policy.

use crate::config::Config;
use axum::http::uri::{Authority, Uri};
use serde::{Deserialize, Deserializer};
use std::fmt;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

/// Overrides `http_token` from the config file when set and non-blank.
pub const TOKEN_ENV: &str = "KONNECT_HTTP_TOKEN";

/// A non-blank bearer token. Never printed: `Debug` redacts it. Compared only
/// through `matches`, so `==` exists only for tests.
#[derive(Clone)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct BearerToken(String);

impl BearerToken {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_string()))
    }

    fn matches(&self, presented: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), presented.as_bytes())
    }
}

/// Visits every byte whatever the first mismatch, so response time does not
/// reveal how long a correct prefix a guess had. Length is not hidden.
fn constant_time_eq(expected: &[u8], presented: &[u8]) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    let diff = expected
        .iter()
        .zip(presented)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    std::hint::black_box(diff) == 0
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerToken([redacted])")
    }
}

impl<'de> Deserialize<'de> for BearerToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        BearerToken::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom("http_token must not be blank"))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HttpPolicyError {
    #[error("http_address {address:?} does not resolve to a socket address: {reason}")]
    Unresolvable { address: String, reason: String },
    #[error(
        "refusing to serve HTTP on non-loopback address {0}: every tool, including \
         ones that write files, would be reachable from the network. Bind \
         127.0.0.1, or set http_allow_remote = true together with http_token \
         (or {TOKEN_ENV})"
    )]
    RemoteNotAllowed(SocketAddr),
    #[error(
        "refusing to serve HTTP on non-loopback address {0} without authentication: \
         http_allow_remote = true also requires http_token (or {TOKEN_ENV})"
    )]
    RemoteWithoutToken(SocketAddr),
}

#[derive(Debug, Clone)]
pub struct HttpPolicy {
    addr: SocketAddr,
    exposure: Exposure,
}

/// A remote bind without a token has no variant.
#[derive(Debug, Clone)]
enum Exposure {
    Loopback { token: Option<BearerToken> },
    Remote { token: BearerToken },
}

impl HttpPolicy {
    pub fn from_config(config: &Config) -> Result<Self, HttpPolicyError> {
        let token = resolve_token(config.http_token.clone(), std::env::var(TOKEN_ENV).ok());
        Self::new(&config.http_address, config.http_allow_remote, token)
    }

    pub fn new(
        address: &str,
        allow_remote: bool,
        token: Option<BearerToken>,
    ) -> Result<Self, HttpPolicyError> {
        let addr = resolve(address)?;
        let exposure = match (addr.ip().is_loopback(), allow_remote, token) {
            (true, _, token) => Exposure::Loopback { token },
            (false, false, _) => return Err(HttpPolicyError::RemoteNotAllowed(addr)),
            (false, true, None) => return Err(HttpPolicyError::RemoteWithoutToken(addr)),
            (false, true, Some(token)) => Exposure::Remote { token },
        };
        Ok(Self { addr, exposure })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn is_remote(&self) -> bool {
        matches!(self.exposure, Exposure::Remote { .. })
    }

    pub fn has_token(&self) -> bool {
        self.token().is_some()
    }

    fn token(&self) -> Option<&BearerToken> {
        match &self.exposure {
            Exposure::Loopback { token } => token.as_ref(),
            Exposure::Remote { token } => Some(token),
        }
    }

    /// The `Host` header must name loopback, or this server's own remote
    /// address, so a DNS-rebound name is refused even without an `Origin`.
    pub fn host_allowed(&self, host: &str) -> bool {
        let Some(authority) = parse_authority(host) else {
            return false;
        };
        let name = authority.host();
        if is_loopback_host(name) {
            return true;
        }
        match self.exposure {
            Exposure::Loopback { .. } => false,
            // A wildcard bind has no single name to compare against. A
            // rebinding page cannot present the token, which a remote bind
            // always requires.
            Exposure::Remote { .. } if self.addr.ip().is_unspecified() => true,
            Exposure::Remote { .. } => parse_ip(name) == Some(self.addr.ip()),
        }
    }

    pub fn authorized(&self, authorization: Option<&str>) -> bool {
        let Some(expected) = self.token() else {
            return true;
        };
        authorization
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .is_some_and(|(_, presented)| expected.matches(presented.trim()))
    }
}

fn resolve(address: &str) -> Result<SocketAddr, HttpPolicyError> {
    let unresolvable = |reason: String| HttpPolicyError::Unresolvable {
        address: address.to_string(),
        reason,
    };
    address
        .to_socket_addrs()
        .map_err(|e| unresolvable(e.to_string()))?
        .next()
        .ok_or_else(|| unresolvable("no addresses".to_string()))
}

pub fn resolve_token(file: Option<BearerToken>, env: Option<String>) -> Option<BearerToken> {
    env.as_deref().and_then(BearerToken::parse).or(file)
}

/// A browser `Origin` is `scheme://host[:port]`. Only http(s) on a loopback
/// host passes; `null`, paths, userinfo and other schemes do not.
pub fn origin_allowed(origin: &str) -> bool {
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    matches!(uri.scheme_str(), Some("http" | "https"))
        && uri.path_and_query().is_none_or(|pq| pq.as_str() == "/")
        && uri
            .authority()
            .and_then(|authority| parse_authority(authority.as_str()))
            .is_some_and(|authority| is_loopback_host(authority.host()))
}

/// Neither header ever legitimately carries userinfo, and `user@host` would
/// otherwise let a lookalike prefix pass.
fn parse_authority(raw: &str) -> Option<Authority> {
    if raw.contains('@') {
        return None;
    }
    raw.parse().ok()
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || parse_ip(host).is_some_and(|ip| ip.is_loopback())
}

fn parse_ip(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(raw: &str) -> BearerToken {
        BearerToken::parse(raw).expect("non-blank token")
    }

    fn loopback() -> HttpPolicy {
        HttpPolicy::new("127.0.0.1:3000", false, None).unwrap()
    }

    #[test]
    fn loopback_origins_are_allowed() {
        for origin in [
            "http://[::1]:3000",
            "http://[::1]",
            "http://127.0.0.1",
            "http://127.0.0.1:3000",
            "http://127.0.0.2:8080",
            "http://localhost:3000",
            "https://localhost",
            "http://LOCALHOST:3000",
        ] {
            assert!(origin_allowed(origin), "{origin} must be allowed");
        }
    }

    #[test]
    fn lookalike_and_malformed_origins_are_rejected() {
        for origin in [
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "http://localhost:3000@evil.com",
            "http://evil.com",
            "http://[::2]:3000",
            "http://128.0.0.1",
            "null",
            "",
            "localhost",
            "http://",
            "file://localhost",
            "ws://localhost:3000",
            "http://local host",
        ] {
            assert!(!origin_allowed(origin), "{origin:?} must be rejected");
        }
    }

    #[test]
    fn loopback_host_headers_are_allowed() {
        let policy = loopback();
        for host in [
            "127.0.0.1:3000",
            "127.0.0.1",
            "localhost:3000",
            "[::1]:3000",
            "127.9.9.9:1",
        ] {
            assert!(policy.host_allowed(host), "{host} must be allowed");
        }
    }

    #[test]
    fn foreign_host_headers_are_rejected_on_a_loopback_bind() {
        let policy = loopback();
        for host in [
            "evil.com",
            "evil.com:3000",
            "localhost.evil.com:3000",
            "192.168.1.10:3000",
            "",
            "a b",
        ] {
            assert!(!policy.host_allowed(host), "{host:?} must be rejected");
        }
    }

    #[test]
    fn a_remote_bind_also_accepts_its_own_address_as_host() {
        let policy = HttpPolicy::new("192.168.1.10:3000", true, Some(token("t"))).unwrap();
        assert!(policy.host_allowed("192.168.1.10:3000"));
        assert!(policy.host_allowed("127.0.0.1:3000"));
        assert!(!policy.host_allowed("evil.com:3000"));
    }

    #[test]
    fn non_loopback_bind_without_opt_in_is_refused() {
        for address in ["0.0.0.0:3000", "192.168.1.10:3000", "[::]:3000"] {
            let err = HttpPolicy::new(address, false, Some(token("t"))).unwrap_err();
            assert!(
                matches!(err, HttpPolicyError::RemoteNotAllowed(_)),
                "{address}: {err:?}"
            );
            assert!(err.to_string().contains("http_allow_remote"), "{err}");
        }
    }

    #[test]
    fn non_loopback_bind_with_opt_in_still_requires_a_token() {
        let err = HttpPolicy::new("0.0.0.0:3000", true, None).unwrap_err();
        assert!(
            matches!(err, HttpPolicyError::RemoteWithoutToken(_)),
            "{err:?}"
        );
        assert!(err.to_string().contains("http_token"), "{err}");
        assert!(err.to_string().contains("KONNECT_HTTP_TOKEN"), "{err}");
    }

    #[test]
    fn loopback_binds_need_neither_opt_in_nor_token() {
        for address in [
            "127.0.0.1:3000",
            "127.0.0.5:3000",
            "[::1]:3000",
            "localhost:3000",
        ] {
            let policy =
                HttpPolicy::new(address, false, None).unwrap_or_else(|e| panic!("{address}: {e}"));
            assert!(policy.addr().ip().is_loopback(), "{address}");
        }
    }

    #[test]
    fn an_unparseable_address_is_an_error() {
        let err = HttpPolicy::new("not an address", false, None).unwrap_err();
        assert!(
            matches!(err, HttpPolicyError::Unresolvable { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_remote_bind_with_opt_in_and_token_is_accepted() {
        let policy = HttpPolicy::new("0.0.0.0:3000", true, Some(token("t"))).unwrap();
        assert!(!policy.addr().ip().is_loopback());
    }

    #[test]
    fn without_a_token_every_request_is_authorized() {
        let policy = loopback();
        assert!(policy.authorized(None));
        assert!(policy.authorized(Some("Bearer anything")));
    }

    #[test]
    fn with_a_token_only_the_matching_bearer_is_authorized() {
        let policy = HttpPolicy::new("127.0.0.1:3000", false, Some(token("s3cret"))).unwrap();
        assert!(policy.authorized(Some("Bearer s3cret")));
        assert!(policy.authorized(Some("bearer s3cret")));
        for header in [
            None,
            Some(""),
            Some("Bearer"),
            Some("Bearer "),
            Some("Bearer s3cre"),
            Some("Bearer s3cret2"),
            Some("Bearer S3CRET"),
            Some("Basic s3cret"),
            Some("s3cret"),
        ] {
            assert!(!policy.authorized(header), "{header:?} must be refused");
        }
    }

    #[test]
    fn a_blank_token_is_not_a_token() {
        assert!(BearerToken::parse("").is_none());
        assert!(BearerToken::parse("  \t").is_none());
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let policy = HttpPolicy::new("127.0.0.1:3000", false, Some(token("s3cret"))).unwrap();
        let rendered = format!("{policy:?} {:?}", token("s3cret"));
        assert!(!rendered.contains("s3cret"), "{rendered}");
    }

    #[test]
    fn the_environment_token_wins_over_the_file_token() {
        assert_eq!(
            resolve_token(Some(token("file")), Some("env".into())),
            Some(token("env"))
        );
        assert_eq!(
            resolve_token(Some(token("file")), Some("  ".into())),
            Some(token("file"))
        );
        assert_eq!(resolve_token(None, None), None);
    }
}
