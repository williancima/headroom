//! Draws a `ShareCard` offscreen (not a capture of the panel) and hands it to the clipboard or a PNG file.
//! The pango context comes from a widget (the panel window works while hidden); the cairo renderer is realized
//! for the default display and never needs a surface.

use crate::share_logic::{Block, ShareCard};
use relm4::gtk::{self, gdk, graphene, gsk, pango, prelude::*};
use std::path::{Path, PathBuf};

const WIDTH: f32 = 360.0;
const INSET: f32 = 16.0;
const SCALE: f32 = 2.0;
const FOOTER: f32 = 40.0;

struct Colors {
    background: gdk::RGBA,
    text: gdk::RGBA,
    dim: gdk::RGBA,
    track: gdk::RGBA,
    fill: gdk::RGBA,
}

impl Colors {
    fn new(dark: bool) -> Self {
        let rgba = |r: f32, g: f32, b: f32, a: f32| gdk::RGBA::new(r, g, b, a);
        if dark {
            Self { background: rgba(0.14, 0.14, 0.14, 1.0), text: rgba(1.0, 1.0, 1.0, 1.0), dim: rgba(1.0, 1.0, 1.0, 0.6), track: rgba(1.0, 1.0, 1.0, 0.15), fill: rgba(0.47, 0.68, 0.93, 1.0) }
        } else {
            Self { background: rgba(0.98, 0.98, 0.98, 1.0), text: rgba(0.18, 0.2, 0.21, 1.0), dim: rgba(0.18, 0.2, 0.21, 0.6), track: rgba(0.0, 0.0, 0.0, 0.12), fill: rgba(0.21, 0.52, 0.89, 1.0) }
        }
    }
}

struct Pen<'a> {
    snapshot: gtk::Snapshot,
    context: pango::Context,
    colors: &'a Colors,
}

impl Pen<'_> {
    fn layout(&self, text: &str, size: f64, semibold: bool, width: f32) -> pango::Layout {
        let layout = pango::Layout::new(&self.context);
        let mut font = self.context.font_description().unwrap_or_default();
        font.set_absolute_size(size * f64::from(pango::SCALE));
        font.set_weight(if semibold { pango::Weight::Semibold } else { pango::Weight::Normal });
        layout.set_font_description(Some(&font));
        layout.set_text(text);
        layout.set_width((width * pango::SCALE as f32) as i32);
        layout.set_wrap(pango::WrapMode::WordChar);
        layout
    }

    fn text(&self, layout: &pango::Layout, x: f32, y: f32, color: &gdk::RGBA) {
        self.snapshot.save();
        self.snapshot.translate(&graphene::Point::new(x, y));
        self.snapshot.append_layout(layout, color);
        self.snapshot.restore();
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32, color: &gdk::RGBA, radius: f32) {
        let rect = graphene::Rect::new(x, y, w, h);
        self.snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, radius));
        self.snapshot.append_color(color, &rect);
        self.snapshot.pop();
    }

    /// One line of text at `y`; returns its height.
    fn single(&self, text: &str, size: f64, semibold: bool, color: &gdk::RGBA, y: f32) -> f32 {
        let layout = self.layout(text, size, semibold, WIDTH - 2.0 * INSET);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        self.text(&layout, INSET, y, color);
        layout.pixel_size().1 as f32
    }

    /// Draws every block from `y` and returns where the content ends.
    fn blocks(&self, card: &ShareCard, mut y: f32) -> f32 {
        let inner = WIDTH - 2.0 * INSET;
        if let Some(title) = &card.title {
            y += self.single(title, 15.0, true, &self.colors.text, y) + 10.0;
        }
        for (i, block) in card.blocks.iter().enumerate() {
            match block {
                Block::Heading(text) => {
                    if i > 0 {
                        y += 10.0;
                    }
                    y += self.single(text, 13.0, true, &self.colors.text, y) + 4.0;
                }
                Block::Line { name, value } => {
                    let value_layout = self.layout(value, 13.0, false, inner);
                    let value_width = if value.is_empty() { 0.0 } else { value_layout.pixel_size().0 as f32 + 8.0 };
                    let name_layout = self.layout(name, 13.0, false, inner - value_width);
                    name_layout.set_ellipsize(pango::EllipsizeMode::End);
                    self.text(&name_layout, INSET, y, &self.colors.text);
                    self.text(&value_layout, WIDTH - INSET - value_layout.pixel_size().0 as f32, y, &self.colors.dim);
                    y += name_layout.pixel_size().1 as f32 + 4.0;
                }
                Block::Meter(fraction) => {
                    self.rect(INSET, y, inner, 6.0, &self.colors.track, 3.0);
                    let filled = inner * fraction.clamp(0.0, 1.0) as f32;
                    if filled > 0.0 {
                        self.rect(INSET, y, filled.max(6.0), 6.0, &self.colors.fill, 3.0);
                    }
                    y += 10.0;
                }
                Block::Text(text) => {
                    let layout = self.layout(text, 11.0, false, inner);
                    self.text(&layout, INSET, y, &self.colors.dim);
                    y += layout.pixel_size().1 as f32 + 4.0;
                }
            }
        }
        y
    }
}

/// The card as a texture: 360 logical points wide at 2x, on an opaque background for the current scheme.
pub fn render(card: &ShareCard, widget: &impl IsA<gtk::Widget>) -> Result<gdk::Texture, String> {
    let colors = Colors::new(relm4::adw::StyleManager::default().is_dark());
    let pen = Pen { snapshot: gtk::Snapshot::new(), context: widget.pango_context(), colors: &colors };
    let content_end = pen.blocks(card, INSET) + INSET;
    // The footer strip: a thin top line, the name on the left and the tagline on the right.
    pen.rect(0.0, content_end, WIDTH, 0.5, &colors.dim, 0.0);
    let name = pen.layout("Headroom", 13.0, true, WIDTH);
    let tagline = pen.layout("Usage limits of your AI coding accounts", 11.0, false, WIDTH);
    let text_y = |h: i32| content_end + (FOOTER - h as f32) / 2.0;
    pen.text(&name, INSET, text_y(name.pixel_size().1), &colors.text);
    pen.text(&tagline, WIDTH - INSET - tagline.pixel_size().0 as f32, text_y(tagline.pixel_size().1), &colors.dim);
    let height = content_end + FOOTER;

    let content = pen.snapshot.to_node().ok_or("the card is empty")?;
    let root = gtk::Snapshot::new();
    root.scale(SCALE, SCALE);
    root.append_color(&colors.background, &graphene::Rect::new(0.0, 0.0, WIDTH, height));
    root.append_node(&content);
    let node = root.to_node().ok_or("the card is empty")?;

    let display = gdk::Display::default().ok_or("no display")?;
    let renderer = gsk::CairoRenderer::new();
    renderer.realize_for_display(&display).map_err(|e| format!("could not start the renderer: {e}"))?;
    let bounds = graphene::Rect::new(0.0, 0.0, WIDTH * SCALE, height * SCALE);
    let texture = renderer.render_texture(&node, Some(&bounds));
    renderer.unrealize();
    Ok(texture)
}

/// Puts the image on the clipboard (the display's, so it works on Wayland while the window is hidden).
pub fn copy(texture: &gdk::Texture) -> Result<(), String> {
    let display = gdk::Display::default().ok_or("no display")?;
    display.clipboard().set_texture(texture);
    Ok(())
}

pub fn write_png(texture: &gdk::Texture, path: &Path) -> Result<(), String> {
    let bytes = texture.save_to_png_bytes();
    std::fs::write(path, bytes.as_ref()).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// `$XDG_RUNTIME_DIR/headroom/share-<request_id>.png`, with the directory created private. The id is already
/// validated by `parse_share_request`.
pub fn share_path(request_id: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("no runtime directory")?;
    let dir = PathBuf::from(runtime).join("headroom");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir.join(format!("share-{request_id}.png")))
}

/// Renders and copies, telling the reader with a friendly message; the detail goes to stderr.
pub fn copy_card(card: &ShareCard, widget: &impl IsA<gtk::Widget>) -> Result<(), String> {
    render(card, widget).and_then(|t| copy(&t)).map_err(|error| {
        eprintln!("{}: could not copy the image: {error}", crate::NAME);
        "Could not copy the image.".to_string()
    })
}

/// Renders to the request's PNG and returns its path; the error is friendly.
pub fn save_card(card: &ShareCard, widget: &impl IsA<gtk::Widget>, request_id: &str) -> Result<String, String> {
    let saved = render(card, widget).and_then(|t| {
        let path = share_path(request_id)?;
        write_png(&t, &path)?;
        Ok(path)
    });
    match saved {
        Ok(path) => Ok(path.to_string_lossy().into_owned()),
        Err(error) => {
            eprintln!("{}: could not make the share image: {error}", crate::NAME);
            Err("Could not make the image.".into())
        }
    }
}
