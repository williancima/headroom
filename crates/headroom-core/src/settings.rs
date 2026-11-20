//! The user's settings (`$XDG_CONFIG_HOME/headroom/settings.json`). The toggle shortcut lives in gsettings, not here.

use crate::http::Proxy;
use serde::{Deserialize, Serialize};

pub const REFRESH_CHOICES: [u32; 5] = [5, 10, 15, 30, 60];
pub const WARNING_RANGE: (i64, i64) = (20, 70);
pub const CRITICAL_RANGE: (i64, i64) = (5, 25);

/// Which notifications are on; all off until the user switches them on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyToggles {
    pub critical: bool,
    pub warning: bool,
    pub burning_fast: bool,
    pub use_it_or_lose_it: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Percent left at or under which a limit is a warning.
    pub warning: i64,
    /// Percent left at or under which a limit is critical; always below `warning`.
    pub critical: i64,
    pub refresh_minutes: u32,
    /// `scheme://host[:port]`; never with credentials.
    pub proxy: Option<String>,
    pub notify: NotifyToggles,
}

impl Default for Settings {
    fn default() -> Self {
        Self { warning: 30, critical: 10, refresh_minutes: 5, proxy: None, notify: NotifyToggles::default() }
    }
}

fn snap(value: i64, (lo, hi): (i64, i64)) -> i64 {
    ((value.clamp(lo, hi) + 2) / 5 * 5).clamp(lo, hi)
}

impl Settings {
    /// Values read from disk may be anything: clamp or default each, and say so on stderr. Never panics.
    pub fn validated(mut self) -> Self {
        let defaults = Self::default();
        let warning = snap(self.warning, WARNING_RANGE);
        let mut critical = snap(self.critical, CRITICAL_RANGE);
        if critical >= warning {
            critical = (warning - 5).clamp(CRITICAL_RANGE.0, CRITICAL_RANGE.1);
        }
        if (warning, critical) != (self.warning, self.critical) {
            eprintln!("headroom: thresholds in settings.json were out of range; using {warning}% and {critical}%");
        }
        self.warning = warning;
        self.critical = critical;
        if !REFRESH_CHOICES.contains(&self.refresh_minutes) {
            eprintln!("headroom: refresh interval in settings.json is not a valid choice; using {} minutes", defaults.refresh_minutes);
            self.refresh_minutes = defaults.refresh_minutes;
        }
        if let Some(raw) = self.proxy.as_deref() {
            if raw.trim().is_empty() {
                self.proxy = None;
            } else if let Err(error) = Proxy::parse(raw) {
                eprintln!("headroom: proxy in settings.json ignored: {error}");
                self.proxy = None;
            }
        }
        self
    }

    /// The parsed proxy, if one is set and valid.
    pub fn parsed_proxy(&self) -> Option<Proxy> {
        self.proxy.as_deref().and_then(|raw| Proxy::parse(raw).ok())
    }

    pub fn refresh_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.refresh_minutes) * 60)
    }

    /// The thresholds the Priority board and the notifications use.
    pub fn priority(&self) -> crate::priority::Settings {
        crate::priority::Settings { warning: self.warning, critical: self.critical, refresh_minutes: i64::from(self.refresh_minutes), ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let s = Settings::default();
        assert_eq!((s.warning, s.critical, s.refresh_minutes, s.proxy.clone()), (30, 10, 5, None));
        assert_eq!(s.notify, NotifyToggles::default());
        assert!(!s.notify.critical && !s.notify.warning && !s.notify.burning_fast && !s.notify.use_it_or_lose_it);
        assert_eq!(s.clone().validated(), s);
    }

    #[test]
    fn missing_fields_default() {
        let s: Settings = serde_json::from_str(r#"{"warning":40,"notify":{"critical":true}}"#).unwrap();
        assert_eq!((s.warning, s.critical, s.refresh_minutes), (40, 10, 5));
        assert!(s.notify.critical && !s.notify.warning);
    }

    #[test]
    fn out_of_range_values_clamp() {
        let s = Settings { warning: 99, critical: -3, refresh_minutes: 7, ..Default::default() }.validated();
        assert_eq!((s.warning, s.critical, s.refresh_minutes), (70, 5, 5));
        let s = Settings { warning: 22, critical: 23, ..Default::default() }.validated();
        assert_eq!((s.warning, s.critical), (20, 15));
    }

    #[test]
    fn critical_stays_below_warning() {
        let s = Settings { warning: 20, critical: 25, ..Default::default() }.validated();
        assert_eq!((s.warning, s.critical), (20, 15));
        let s = Settings { warning: 25, critical: 25, ..Default::default() }.validated();
        assert!(s.critical < s.warning);
    }

    #[test]
    fn bad_proxy_is_dropped_and_good_one_kept() {
        let s = Settings { proxy: Some("socks5://user:pw@host".into()), ..Default::default() }.validated();
        assert_eq!(s.proxy, None);
        let s = Settings { proxy: Some("socks5://localhost:9050".into()), ..Default::default() }.validated();
        assert_eq!(s.proxy.as_deref(), Some("socks5://localhost:9050"));
        assert!(s.parsed_proxy().is_some());
    }
}
