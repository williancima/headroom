//! Provider icons: the extension's `headroom-provider-<id>-symbolic.svg` files, bundled as a GResource.

use relm4::gtk::{self, gio};

/// Makes the bundled icons known to the icon theme; call once GTK is up.
pub fn register() {
    gio::resources_register_include!("icons.gresource").expect("could not register the icons");
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path("/dev/headroom/Headroom/icons");
    }
}

/// The icon of a provider (`ProviderKind::id()`, or `cliproxyapi` for a hub).
pub fn provider_icon(kind: &str) -> gtk::Image {
    gtk::Image::builder()
        .icon_name(format!("headroom-provider-{kind}-symbolic"))
        .pixel_size(16)
        .build()
}
