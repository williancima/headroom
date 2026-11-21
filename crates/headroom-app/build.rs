//! Bundles `extension/icons/headroom-provider-*.svg` as a GResource, so the app and the extension share one copy.

use std::{env, fs, path::PathBuf};

fn main() {
    let icons =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../extension/icons");
    println!("cargo:rerun-if-changed={}", icons.display());
    let mut names: Vec<String> = fs::read_dir(&icons)
        .expect("extension/icons is missing")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("headroom-provider-") && n.ends_with(".svg"))
        .collect();
    names.sort();
    let files: String = names.iter().map(|n| format!("<file>{n}</file>")).collect();
    let xml = format!(
        r#"<gresources><gresource prefix="/dev/headroom/Headroom/icons/scalable/apps">{files}</gresource></gresources>"#
    );
    let xml_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("icons.gresource.xml");
    fs::write(&xml_path, xml).unwrap();
    glib_build_tools::compile_resources(
        &[icons.to_str().unwrap()],
        xml_path.to_str().unwrap(),
        "icons.gresource",
    );
}
