//! Where a Copilot seat signs in: github.com, or a company's GitHub Enterprise Cloud domain on GHE.com.
//! Self-hosted GitHub Enterprise Server is not supported.
pub const INVALID_MESSAGE: &str = "Headroom works with GitHub Enterprise on GHE.com. Enter an address like octocorp.ghe.com.";
/// A saved domain that fails the same checks as a typed one; the token is never sent to it.
pub const INVALID_SAVED_MESSAGE: &str = "This account's GitHub Enterprise domain is not valid. Remove the account and add it again.";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GitHubDomain {
    #[default]
    Github,
    /// A `<name>.ghe.com` domain.
    Enterprise(String),
}

impl GitHubDomain {
    /// The domain an account saved; none is github.com. accounts.json is re-checked with the rules of
    /// `parse`: anything but an exact `<name>.ghe.com` is `None`, so the token goes nowhere.
    pub fn from_saved(saved: Option<&str>) -> Option<Self> {
        match saved {
            None => Some(Self::Github),
            Some(d) => Self::parse(d).filter(|parsed| parsed.saved() == Some(d)),
        }
    }

    /// Reads a typed or pasted address. It trims and lowercases it and drops the scheme and path, so a
    /// pasted link works. Blank or github.com is github.com, and a bare name is `<name>.ghe.com`. Any
    /// other address is not a GHE.com domain (`None`).
    pub fn parse(input: &str) -> Option<Self> {
        let lower = input.trim().to_lowercase();
        let rest = lower.split_once("://").map_or(lower.as_str(), |(_, r)| r);
        let mut host = rest.split(['/', '?', '#']).next().unwrap_or_default().to_string();
        if host.is_empty() || host == "github.com" || host == "www.github.com" {
            return Some(Self::Github);
        }
        if !host.contains('.') {
            host.push_str(".ghe.com");
        }
        let name = host.strip_suffix(".ghe.com")?;
        let valid = !name.is_empty()
            && !name.starts_with('-')
            && !name.ends_with('-')
            && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        valid.then_some(Self::Enterprise(host))
    }

    /// What an account saves: github.com saves none.
    pub fn saved(&self) -> Option<&str> {
        match self {
            Self::Github => None,
            Self::Enterprise(host) => Some(host),
        }
    }

    pub fn host(&self) -> &str {
        self.saved().unwrap_or("github.com")
    }

    pub fn web_url(&self, path: &str) -> String {
        format!("https://{}{path}", self.host())
    }

    /// GitHub's API lives on `api.` of the same domain: api.github.com, or api.<name>.ghe.com.
    pub fn api_url(&self, path: &str) -> String {
        format!("https://api.{}{path}", self.host())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enterprise() -> Option<GitHubDomain> {
        Some(GitHubDomain::Enterprise("octocorp.ghe.com".into()))
    }

    #[test]
    fn the_domain_is_normalized_and_only_ghe_com_is_accepted() {
        for blank in ["", "   ", "github.com", " GitHub.com ", "https://github.com/settings/copilot", "www.github.com"] {
            assert_eq!(GitHubDomain::parse(blank), Some(GitHubDomain::Github), "{blank}");
        }
        for typed in ["octocorp", "OctoCorp", " octocorp.ghe.com ", "https://octocorp.ghe.com/login/device", "octocorp.ghe.com/?x=1", "octocorp.GHE.com#a"] {
            assert_eq!(GitHubDomain::parse(typed), enterprise(), "{typed}");
        }
        assert_eq!(GitHubDomain::parse("octo-corp-2"), Some(GitHubDomain::Enterprise("octo-corp-2.ghe.com".into())));
        for invalid in ["github.acme.com", "ghe.com", ".ghe.com", "a.b.ghe.com", "-octo.ghe.com", "octo-.ghe.com", "octo corp", "octo_corp.ghe.com", "octocorp.ghe.com:8443"] {
            assert_eq!(GitHubDomain::parse(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn hosts_and_saved_form() {
        let github = GitHubDomain::from_saved(None).unwrap();
        assert_eq!((github.saved(), github.web_url("/x"), github.api_url("/user")), (None, "https://github.com/x".into(), "https://api.github.com/user".into()));
        let ghe = GitHubDomain::from_saved(Some("octocorp.ghe.com")).unwrap();
        assert_eq!((ghe.saved(), ghe.web_url("/x"), ghe.api_url("/user")), (Some("octocorp.ghe.com"), "https://octocorp.ghe.com/x".into(), "https://api.octocorp.ghe.com/user".into()));
    }

    #[test]
    fn a_tampered_saved_domain_is_refused() {
        for bad in ["evil.example", "octocorp.ghe.com.evil.example", "evil.example/x.ghe.com", "OctoCorp.ghe.com", " octocorp.ghe.com", "octocorp", "github.com", "", "a.b.ghe.com", "octocorp.ghe.com:8443"] {
            assert_eq!(GitHubDomain::from_saved(Some(bad)), None, "{bad}");
        }
    }
}
