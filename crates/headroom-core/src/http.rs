//! The one place provider HTTP clients start from, so the user's proxy applies to all of them.

use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Socks5,
    Http,
    Https,
}

impl Scheme {
    fn default_port(self) -> u16 {
        match self {
            Self::Socks5 => 1080,
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// Why a proxy URL was refused. The text never quotes the URL or any part of it: a password can sit anywhere in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    #[error("The proxy must look like socks5://host:port, http://host:port or https://host:port")]
    Invalid,
    #[error("The proxy must not contain a user name or password; Headroom does not store proxy credentials")]
    Credentials,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
}

impl Proxy {
    pub fn parse(raw: &str) -> Result<Self, ProxyError> {
        let raw = raw.trim();
        // Any `@` means user info (or something we can't read safely), wherever it sits.
        if raw.contains('@') {
            return Err(ProxyError::Credentials);
        }
        let (scheme, rest) = raw.split_once("://").ok_or(ProxyError::Invalid)?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "socks5" => Scheme::Socks5,
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            _ => return Err(ProxyError::Invalid),
        };
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
            let (host, tail) = v6.split_once(']').ok_or(ProxyError::Invalid)?;
            let port = match tail {
                "" => None,
                t => Some(t.strip_prefix(':').ok_or(ProxyError::Invalid)?),
            };
            (format!("[{host}]"), port)
        } else {
            match rest.split_once(':') {
                Some((host, port)) => (host.to_string(), Some(port)),
                None => (rest.to_string(), None),
            }
        };
        let bare = host.trim_matches(|c| c == '[' || c == ']');
        let host_ok = !bare.is_empty()
            && bare.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') || (host.starts_with('[') && c == ':'));
        let port = match port {
            None => scheme.default_port(),
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse::<u16>().map_err(|_| ProxyError::Invalid)?,
            Some(_) => return Err(ProxyError::Invalid),
        };
        if !host_ok || port == 0 {
            return Err(ProxyError::Invalid);
        }
        Ok(Self { scheme, host, port })
    }

    /// The URL reqwest takes. SOCKS resolves names at the proxy (`socks5h`) so lookups don't bypass it.
    fn url(&self) -> String {
        let scheme = match self.scheme {
            Scheme::Socks5 => "socks5h",
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        format!("{scheme}://{}:{}", self.host, self.port)
    }
}

static PROXY: RwLock<Option<Proxy>> = RwLock::new(None);

/// Sets the proxy every client built from now on uses; `None` goes direct. Clients that live for the whole
/// process (the hub client) must be rebuilt by the caller.
pub fn set_proxy(proxy: Option<Proxy>) {
    *PROXY.write().unwrap_or_else(|e| e.into_inner()) = proxy;
}

pub fn client_builder() -> reqwest::ClientBuilder {
    with_proxy(reqwest::Client::builder(), PROXY.read().unwrap_or_else(|e| e.into_inner()).clone())
}

fn with_proxy(builder: reqwest::ClientBuilder, proxy: Option<Proxy>) -> reqwest::ClientBuilder {
    let Some(proxy) = proxy else { return builder };
    match reqwest::Proxy::all(proxy.url()) {
        Ok(p) => builder.proxy(p),
        Err(_) => {
            eprintln!("headroom: the proxy could not be applied; connecting directly");
            builder
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_schemes_and_default_ports() {
        assert_eq!(Proxy::parse("socks5://localhost").unwrap(), Proxy { scheme: Scheme::Socks5, host: "localhost".into(), port: 1080 });
        assert_eq!(Proxy::parse("http://proxy.local").unwrap().port, 80);
        assert_eq!(Proxy::parse("HTTPS://proxy.local/").unwrap().port, 443);
        assert_eq!(Proxy::parse("http://10.0.0.1:8080").unwrap().port, 8080);
        assert_eq!(Proxy::parse("socks5://[::1]:9050").unwrap().host, "[::1]");
    }

    #[test]
    fn rejects_bad_urls() {
        for raw in ["", "localhost:1080", "ftp://h", "socks5://", "http://h:0", "http://h:99999", "http://h:x", "http://h/path", "http://h?q=1", "http://ho st"] {
            assert_eq!(Proxy::parse(raw), Err(ProxyError::Invalid), "{raw}");
        }
    }

    #[test]
    fn rejects_credentials_without_echoing_them() {
        for raw in ["socks5://user:hunter2@host:1080", "http://user@host", "user:hunter2@host:1080", "http://host/@x"] {
            let error = Proxy::parse(raw).unwrap_err();
            assert_eq!(error, ProxyError::Credentials, "{raw}");
        }
        for raw in ["socks5://user:hunter2@localhost:1080", "hunter2", "ftp://hunter2:x"] {
            let text = Proxy::parse(raw).unwrap_err().to_string();
            assert!(!text.contains("hunter2") && !text.contains("ftp:") && !text.contains("localhost"), "{text}");
        }
    }

    #[test]
    fn client_builder_builds_with_and_without_proxy() {
        for raw in ["socks5://127.0.0.1:1", "http://proxy.local", "https://proxy.local:8443"] {
            assert!(with_proxy(reqwest::Client::builder(), Some(Proxy::parse(raw).unwrap())).build().is_ok(), "{raw}");
        }
        assert!(client_builder().build().is_ok());
    }
}
