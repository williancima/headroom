//! Pure parts of the startup setup: AppImage wrapper script, extension file sync, restart check.

use std::collections::BTreeMap;
use std::path::Path;

pub const EXTENSION_UUID: &str = "headroom@headroom.dev";
const WRAPPER_MARKER: &str = "# Written by the Headroom AppImage";

/// The AppImage path to run: `$APPIMAGE` when it names an existing file, else the running binary.
pub fn exe_choice<'a>(appimage: Option<&'a Path>, appimage_exists: bool, current: &'a Path) -> &'a Path {
    match appimage {
        Some(path) if appimage_exists => path,
        _ => current,
    }
}

/// `/proc/self/exe` of a binary replaced on disk reads `<path> (deleted)`; the path without that suffix.
pub fn strip_deleted(link: &str) -> Option<&str> {
    link.strip_suffix(" (deleted)")
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// `~/.local/bin/headroom` for an AppImage install: the extension runs it to read the usage.
pub fn wrapper_script(appimage: &Path) -> String {
    format!("#!/bin/sh\n{WRAPPER_MARKER}\nexec {} --cli \"$@\"\n", shell_quote(&appimage.to_string_lossy()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum WrapperAction {
    Write,
    UpToDate,
    /// Something else lives there (a real binary, the user's own script): never touched.
    Foreign,
}

pub fn wrapper_action(existing: Option<&str>, wanted: &str) -> WrapperAction {
    match existing {
        None => WrapperAction::Write,
        Some(text) if text == wanted => WrapperAction::UpToDate,
        Some(text) if text.starts_with("#!/bin/sh\n") && text.lines().nth(1) == Some(WRAPPER_MARKER) => WrapperAction::Write,
        Some(_) => WrapperAction::Foreign,
    }
}

/// Files by path relative to the extension folder.
pub type Files = BTreeMap<String, Vec<u8>>;

/// What to copy (new or changed) and what to remove (no longer shipped) so `installed` matches `shipped`.
pub fn sync_plan(shipped: &Files, installed: &Files) -> (Vec<String>, Vec<String>) {
    let copy = shipped.iter().filter(|(path, bytes)| installed.get(*path) != Some(*bytes)).map(|(path, _)| path.clone()).collect();
    let remove = installed.keys().filter(|path| !shipped.contains_key(*path)).cloned().collect();
    (copy, remove)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(items: &[(&str, &str)]) -> Files {
        items.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect()
    }

    #[test]
    fn exe_prefers_an_existing_appimage() {
        let (image, current) = (Path::new("/home/u/Headroom.AppImage"), Path::new("/tmp/.mount_x/usr/bin/headroom-app"));
        assert_eq!(exe_choice(Some(image), true, current), image);
        assert_eq!(exe_choice(Some(image), false, current), current);
        assert_eq!(exe_choice(None, false, current), current);
    }

    #[test]
    fn deleted_binary_is_detected_and_stripped() {
        assert_eq!(strip_deleted("/usr/bin/headroom-app (deleted)"), Some("/usr/bin/headroom-app"));
        assert_eq!(strip_deleted("/usr/bin/headroom-app"), None);
    }

    #[test]
    fn wrapper_runs_the_appimage_in_cli_mode_and_quotes_the_path() {
        let text = wrapper_script(Path::new("/home/u/My Apps/it's.AppImage"));
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.ends_with("exec '/home/u/My Apps/it'\\''s.AppImage' --cli \"$@\"\n"));
    }

    #[test]
    fn wrapper_is_rewritten_only_when_it_is_ours() {
        let wanted = wrapper_script(Path::new("/new/Headroom.AppImage"));
        let old = wrapper_script(Path::new("/old/Headroom.AppImage"));
        assert_eq!(wrapper_action(None, &wanted), WrapperAction::Write);
        assert_eq!(wrapper_action(Some(&wanted), &wanted), WrapperAction::UpToDate);
        assert_eq!(wrapper_action(Some(&old), &wanted), WrapperAction::Write);
        assert_eq!(wrapper_action(Some("\u{7f}ELF binary"), &wanted), WrapperAction::Foreign);
        assert_eq!(wrapper_action(Some("#!/bin/sh\nexec mine\n"), &wanted), WrapperAction::Foreign);
    }

    #[test]
    fn sync_copies_new_and_changed_and_removes_stale_files() {
        let shipped = files(&[("extension.js", "v2"), ("metadata.json", "m"), ("icons/a.svg", "a")]);
        let installed = files(&[("extension.js", "v1"), ("metadata.json", "m"), ("old.js", "x")]);
        assert_eq!(sync_plan(&shipped, &installed), (vec!["extension.js".to_string(), "icons/a.svg".to_string()], vec!["old.js".to_string()]));
        assert_eq!(sync_plan(&shipped, &shipped), (vec![], vec![]));
    }
}
