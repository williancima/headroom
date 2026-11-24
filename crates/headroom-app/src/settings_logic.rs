//! Pure parts of the Settings window: autostart file, shortcut list edits, threshold limits and proxy input.

use headroom_core::http::Proxy;
use std::path::{Path, PathBuf};

pub const DESKTOP_FILE: &str = "dev.headroom.Headroom.desktop";
const DESKTOP_TEMPLATE: &str = include_str!("../../../data/dev.headroom.Headroom.desktop");

/// `$XDG_CONFIG_HOME/autostart/dev.headroom.Headroom.desktop`; whether it exists is the switch's state.
pub fn autostart_path(config_dir: &Path) -> PathBuf {
    config_dir.join("autostart").join(DESKTOP_FILE)
}

/// One argument of an `Exec` line, quoted per the Desktop Entry spec.
fn exec_argument(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    if !arg.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`".contains(c)) {
        return arg;
    }
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    // The file's own string escaping doubles every backslash again.
    quoted.replace('\\', "\\\\")
}

/// The desktop file with `Exec=` set to this binary, started by the session.
pub fn autostart_contents(exe: &Path) -> String {
    let mut text: String = DESKTOP_TEMPLATE
        .lines()
        .map(|line| if line.starts_with("Exec=") { autostart_exec(exe) } else { line.to_string() })
        .collect::<Vec<_>>()
        .join("\n");
    text.push_str("\nX-GNOME-Autostart-enabled=true\n");
    text
}

/// True when the autostart file's `Exec` is not this binary (it moved, or another install took over).
pub fn autostart_stale(contents: &str, exe: &Path) -> bool {
    let wanted = autostart_exec(exe);
    !contents.lines().any(|line| line == wanted)
}

/// `--background`: a login start that finds Headroom already running leaves it alone.
fn autostart_exec(exe: &Path) -> String {
    format!("Exec={} --background", exec_argument(&exe.to_string_lossy()))
}

pub const KEYBINDING_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/headroom-toggle/";
pub const TOGGLE_COMMAND: &str = "gdbus call --session --dest dev.headroom.Headroom --object-path /dev/headroom/Headroom --method org.gtk.Actions.Activate toggle [] {}";

/// The list with `path` appended, or `None` when it is already there. Other entries are left as they are.
pub fn with_path(list: &[String], path: &str) -> Option<Vec<String>> {
    (!list.iter().any(|p| p == path)).then(|| list.iter().cloned().chain([path.to_string()]).collect())
}

/// The list without `path`, or `None` when it is not there.
pub fn without_path(list: &[String], path: &str) -> Option<Vec<String>> {
    list.iter().any(|p| p == path).then(|| list.iter().filter(|p| *p != path).cloned().collect())
}

/// Critical stays below Warning: the highest Critical for a Warning.
pub fn critical_max(warning: i64) -> i64 {
    (warning - 5).min(25)
}

/// What the Proxy row saved: empty is off; anything else must parse. The error text never quotes the input.
pub fn proxy_input(text: &str) -> Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    Proxy::parse(text).map(|_| Some(text.to_string())).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn autostart_lives_in_the_config_autostart_folder() {
        assert_eq!(autostart_path(Path::new("/home/u/.config")), Path::new("/home/u/.config/autostart/dev.headroom.Headroom.desktop"));
    }

    #[test]
    fn autostart_file_runs_this_binary_and_is_enabled() {
        let text = autostart_contents(Path::new("/opt/headroom/headroom-app"));
        assert!(text.contains("\nExec=/opt/headroom/headroom-app --background\n"));
        assert!(text.contains("Name=Headroom") && text.contains("Type=Application"));
        assert_eq!(text.matches("Exec=").count(), 1);
        assert!(text.ends_with("X-GNOME-Autostart-enabled=true\n"));
    }

    #[test]
    fn autostart_is_stale_only_when_exec_differs() {
        let text = autostart_contents(Path::new("/old/Headroom.AppImage"));
        assert!(!autostart_stale(&text, Path::new("/old/Headroom.AppImage")));
        assert!(autostart_stale(&text, Path::new("/usr/bin/headroom-app")));
        assert!(autostart_stale("[Desktop Entry]\n", Path::new("/usr/bin/headroom-app")));
        // Written before `--background`: rewritten, or a login start could open the window.
        assert!(autostart_stale("Exec=/usr/bin/headroom-app\n", Path::new("/usr/bin/headroom-app")));
    }

    #[test]
    fn exec_paths_are_quoted_when_needed() {
        assert_eq!(exec_argument("/a b/headroom-app"), "\"/a b/headroom-app\"");
        assert_eq!(exec_argument("/a/100%/x"), "/a/100%%/x");
        assert_eq!(exec_argument("/a/$x y"), "\"/a/\\\\$x y\"");
    }

    #[test]
    fn our_keybinding_is_appended_once_and_others_never_move() {
        let others = list(&["/x/custom1/", "/x/custom0/"]);
        let added = with_path(&others, KEYBINDING_PATH).unwrap();
        assert_eq!(added, list(&["/x/custom1/", "/x/custom0/", KEYBINDING_PATH]));
        assert_eq!(with_path(&added, KEYBINDING_PATH), None);
    }

    #[test]
    fn clearing_removes_only_our_path() {
        let all = list(&["/x/custom1/", KEYBINDING_PATH, "/x/custom0/"]);
        assert_eq!(without_path(&all, KEYBINDING_PATH).unwrap(), list(&["/x/custom1/", "/x/custom0/"]));
        assert_eq!(without_path(&list(&["/x/custom1/"]), KEYBINDING_PATH), None);
    }

    #[test]
    fn critical_stays_below_warning() {
        assert_eq!(critical_max(20), 15);
        assert_eq!(critical_max(30), 25);
        assert_eq!(critical_max(70), 25);
    }

    #[test]
    fn proxy_input_is_off_valid_or_a_friendly_error_without_the_url() {
        assert_eq!(proxy_input("  "), Ok(None));
        assert_eq!(proxy_input("socks5://localhost:9050"), Ok(Some("socks5://localhost:9050".into())));
        let error = proxy_input("http://bob:hunter2@proxy").unwrap_err();
        assert!(!error.contains("hunter2") && !error.contains("bob"));
        assert!(proxy_input("nonsense").is_err());
    }
}
