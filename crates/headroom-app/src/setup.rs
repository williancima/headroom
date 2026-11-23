//! Startup setup and self-maintenance: autostart path, AppImage extension and `headroom` command,
//! enabling the extension once, restarting when the binary was replaced, and AppImage self-update.

use crate::setup_logic::*;
use crate::settings_logic::{autostart_contents, autostart_path, autostart_stale, with_path};
use crate::{Command, NAME};
use headroom_core::update;
use relm4::gtk::{gio, glib, prelude::*};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const STARTUP_DELAY: Duration = Duration::from_secs(60);
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const DELETED_CHECK_EVERY: Duration = Duration::from_secs(60);

/// What starts this app: the AppImage file when running from one, else the binary itself.
pub fn app_exe() -> std::io::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let current = current.to_str().and_then(strip_deleted).map(PathBuf::from).unwrap_or(current);
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let exists = appimage.as_deref().is_some_and(Path::is_file);
    Ok(exe_choice(appimage.as_deref(), exists, &current).to_path_buf())
}

/// `(appimage file, its mount)` when running from an AppImage.
fn appimage() -> Option<(PathBuf, PathBuf)> {
    let (image, mount) = (PathBuf::from(std::env::var_os("APPIMAGE")?), PathBuf::from(std::env::var_os("APPDIR")?));
    image.is_file().then_some((image, mount))
}

/// Runs once per launch, on the main thread of the primary instance. Every failure is logged and skipped.
pub fn run_startup() {
    refresh_autostart();
    if let Some((image, mount)) = appimage() {
        install_extension(&mount);
        install_wrapper(&image);
        install_dbus_service(&image);
    }
    enable_extension_once();
}

fn refresh_autostart() {
    let (Ok(exe), path) = (app_exe(), autostart_path(&glib::user_config_dir())) else { return };
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    if autostart_stale(&text, &exe) {
        match std::fs::write(&path, autostart_contents(&exe)) {
            Ok(()) => eprintln!("{NAME}: Launch at Login now points to {}", exe.display()),
            Err(error) => eprintln!("{NAME}: could not update Launch at Login: {error}"),
        }
    }
}

fn read_tree(root: &Path, dir: &Path, into: &mut Files) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            read_tree(root, &path, into)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            into.insert(relative.to_string_lossy().into_owned(), std::fs::read(&path)?);
        }
    }
    Ok(())
}

fn tree(dir: &Path) -> std::io::Result<Files> {
    let mut files = Files::new();
    match read_tree(dir, dir, &mut files) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Files::new()),
        result => result.map(|()| files),
    }
}

fn user_extension_dir() -> PathBuf {
    glib::user_data_dir().join("gnome-shell/extensions").join(EXTENSION_UUID)
}

fn install_extension(mount: &Path) {
    let from = mount.join("usr/share/gnome-shell/extensions").join(EXTENSION_UUID);
    let to = user_extension_dir();
    let result = (|| -> std::io::Result<usize> {
        let (shipped, installed) = (tree(&from)?, tree(&to)?);
        let (copy, remove) = sync_plan(&shipped, &installed);
        for path in &copy {
            let target = to.join(path);
            std::fs::create_dir_all(target.parent().unwrap_or(&to))?;
            std::fs::write(target, &shipped[path])?;
        }
        for path in &remove {
            std::fs::remove_file(to.join(path))?;
        }
        Ok(copy.len() + remove.len())
    })();
    match result {
        Ok(0) => {}
        Ok(changed) => eprintln!("{NAME}: updated the GNOME Shell extension ({changed} files); log out and back in to load it"),
        Err(error) => eprintln!("{NAME}: could not install the GNOME Shell extension: {error}"),
    }
}

/// Lets the session bus start the AppImage when the extension talks to it.
fn install_dbus_service(image: &Path) {
    let path = glib::user_data_dir().join("dbus-1/services/dev.headroom.Headroom.service");
    let wanted = dbus_service(image);
    if std::fs::read_to_string(&path).is_ok_and(|text| text == wanted) {
        return;
    }
    let written = std::fs::create_dir_all(path.parent().unwrap_or(&path)).and_then(|()| std::fs::write(&path, wanted));
    match written {
        Ok(()) => eprintln!("{NAME}: wrote the D-Bus service at {}", path.display()),
        Err(error) => eprintln!("{NAME}: could not write the D-Bus service: {error}"),
    }
}

fn install_wrapper(image: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let path = home.join(".local/bin/headroom");
    let wanted = wrapper_script(image);
    let existing = std::fs::read(&path).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
    match wrapper_action(existing.as_deref(), &wanted) {
        WrapperAction::UpToDate => {}
        WrapperAction::Foreign => eprintln!("{NAME}: {} exists and is not ours; leaving it alone", path.display()),
        WrapperAction::Write => {
            let written = std::fs::create_dir_all(path.parent().unwrap_or(&home))
                .and_then(|()| std::fs::write(&path, wanted))
                .and_then(|()| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)));
            match written {
                Ok(()) => eprintln!("{NAME}: wrote the headroom command at {}", path.display()),
                Err(error) => eprintln!("{NAME}: could not write the headroom command: {error}"),
            }
        }
    }
}

/// Adds the extension to GNOME's enabled list the first time it is installed; never again, since the user may
/// have switched it off.
pub fn enable_extension_once() {
    let installed = Path::new("/usr/share/gnome-shell/extensions").join(EXTENSION_UUID).join("metadata.json").is_file()
        || user_extension_dir().join("metadata.json").is_file();
    let state = dirs_state().join("extension_enabled");
    if !installed || state.exists() {
        return;
    }
    let has_schema = gio::SettingsSchemaSource::default().is_some_and(|s| s.lookup("org.gnome.shell", true).is_some());
    if !has_schema {
        return;
    }
    let shell = gio::Settings::new("org.gnome.shell");
    let list: Vec<String> = shell.strv("enabled-extensions").iter().map(|s| s.to_string()).collect();
    if let Some(next) = with_path(&list, EXTENSION_UUID) {
        if let Err(error) = shell.set_strv("enabled-extensions", next) {
            return eprintln!("{NAME}: could not enable the GNOME Shell extension: {error}");
        }
        gio::Settings::sync();
        eprintln!("{NAME}: enabled the GNOME Shell extension (if it doesn't appear, log out and back in)");
    }
    let marked = std::fs::create_dir_all(state.parent().unwrap_or(&state)).and_then(|()| std::fs::write(&state, b""));
    if let Err(error) = marked {
        eprintln!("{NAME}: could not save that the extension was enabled: {error}");
    }
}

/// Same folder as the store's state files.
fn dirs_state() -> PathBuf {
    glib::user_state_dir().join("headroom")
}

/// Starts a fresh instance once this process is gone. Detached, so it outlives the quit.
fn restart() {
    use std::os::unix::process::CommandExt;
    let script = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$0""#;
    let spawned = app_exe().and_then(|exe| {
        std::process::Command::new("sh")
            .args(["-c", script])
            .arg(exe)
            .arg(std::process::id().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
    });
    if let Err(error) = spawned {
        eprintln!("{NAME}: could not restart: {error}");
    }
}

/// Restarts into the new binary and quits through the normal path.
fn restart_and_quit(commands: &UnboundedSender<Command>) {
    restart();
    let _ = commands.send(Command::Quit);
}

async fn watch_replaced_binary(commands: UnboundedSender<Command>) {
    loop {
        tokio::time::sleep(DELETED_CHECK_EVERY).await;
        let link = std::fs::read_link("/proc/self/exe").ok();
        if link.as_deref().and_then(Path::to_str).and_then(strip_deleted).is_some() {
            eprintln!("{NAME}: the app was updated on disk; restarting");
            return restart_and_quit(&commands);
        }
    }
}

/// Downloads the newer AppImage next to the old one and swaps it in. `Ok(true)` when it was replaced.
async fn update_once(image: &Path) -> Result<bool, String> {
    let release = update::latest_release(update::LATEST_URL).await?;
    let Some(found) = update::pick(&release, env!("CARGO_PKG_VERSION")) else { return Ok(false) };
    eprintln!("{NAME}: downloading version {}", found.version);
    let bytes = update::download(&found).await?;
    swap_in(image, &bytes).map(|()| true).map_err(|e| format!("could not replace the AppImage: {e}"))
}

/// Writes `<image>.part` as a new file (never through a planted symlink), flushes it to disk and renames it
/// over `image`. A stale `.part` is removed first; a failed swap removes the new one.
fn swap_in(image: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut part = image.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    match std::fs::remove_file(&part) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let swapped = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o755).open(&part).and_then(|mut file| {
        file.write_all(bytes)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        file.sync_all()
    });
    let swapped = swapped.and_then(|()| std::fs::rename(&part, image));
    if swapped.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    swapped
}

async fn self_update(image: PathBuf, commands: UnboundedSender<Command>) {
    tokio::time::sleep(STARTUP_DELAY).await;
    loop {
        match update_once(&image).await {
            Ok(true) => {
                eprintln!("{NAME}: updated the AppImage; restarting");
                return restart_and_quit(&commands);
            }
            Ok(false) => {}
            Err(message) => eprintln!("{NAME}: could not check for updates: {message}"),
        }
        tokio::time::sleep(UPDATE_EVERY).await;
    }
}

/// Starts the restart watcher and, for an AppImage in a writable folder, the self-update. Call inside the runtime.
pub fn spawn_background(commands: UnboundedSender<Command>) {
    tokio::spawn(watch_replaced_binary(commands.clone()));
    let Some((image, _)) = appimage() else { return };
    let dir = image.parent().unwrap_or(Path::new("."));
    let probe = dir.join(".headroom-write-test");
    if std::fs::write(&probe, b"").and_then(|()| std::fs::remove_file(&probe)).is_err() {
        return eprintln!("{NAME}: {} is not writable, so the AppImage will not update itself", dir.display());
    }
    tokio::spawn(self_update(image, commands));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deb_login_autostart_enables_the_extension_only_where_the_app_exists() {
        let entry = include_str!("../../../data/dev.headroom.Headroom-extension.desktop");
        assert!(entry.lines().any(|l| l == "Exec=headroom-app --enable-extension"));
        assert!(entry.lines().any(|l| l == "TryExec=headroom-app"));
        assert!(entry.lines().any(|l| l == "OnlyShowIn=GNOME;"));
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains(r#"["../../data/dev.headroom.Headroom-extension.desktop", "etc/xdg/autostart/", "644"]"#));
    }

    #[test]
    fn the_new_appimage_replaces_the_old_one_without_following_a_planted_part() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("headroom-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (image, victim) = (dir.join("Headroom.AppImage"), dir.join("victim"));
        std::fs::write(&image, b"old").unwrap();
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("Headroom.AppImage.part")).unwrap();
        swap_in(&image, b"new").unwrap();
        assert_eq!(std::fs::read(&image).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&image).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(std::fs::symlink_metadata(dir.join("Headroom.AppImage.part")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
