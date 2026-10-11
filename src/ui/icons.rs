//! Application icons: loaded on a background thread on first use and cached as textures.

use crate::platform;
use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

enum Slot {
    Pending,
    Missing,
    Ready(TextureHandle),
}

struct Loader {
    requests: Sender<Arc<str>>,
    results: Receiver<(Arc<str>, Option<ColorImage>)>,
}

#[derive(Default)]
pub struct Icons {
    loader: RefCell<Option<Loader>>,
    slots: RefCell<HashMap<Arc<str>, Slot>>,
}

impl Icons {
    /// Returns the icon of `exe` if it is loaded; otherwise queues it (once).
    pub fn get(&self, ctx: &Context, exe: &Arc<str>) -> Option<TextureHandle> {
        if !platform::HAS_EXE_ICONS || exe.is_empty() {
            return None;
        }
        let mut slots = self.slots.borrow_mut();
        match slots.get(exe) {
            Some(Slot::Ready(t)) => return Some(t.clone()),
            Some(_) => return None,
            None => {}
        }
        let mut loader = self.loader.borrow_mut();
        let loader = loader.get_or_insert_with(|| spawn(ctx.clone()));
        if loader.requests.send(exe.clone()).is_ok() {
            slots.insert(exe.clone(), Slot::Pending);
        } else {
            slots.insert(exe.clone(), Slot::Missing);
        }
        None
    }

    /// Turns finished loads into textures. Call once per frame.
    pub fn pump(&self, ctx: &Context) {
        let loader = self.loader.borrow();
        let Some(loader) = loader.as_ref() else {
            return;
        };
        let mut slots = self.slots.borrow_mut();
        for (exe, image) in loader.results.try_iter() {
            let slot = match image {
                Some(img) => Slot::Ready(ctx.load_texture(
                    format!("icon:{exe}"),
                    img,
                    TextureOptions {
                        mipmap_mode: Some(egui::TextureFilter::Linear),
                        ..TextureOptions::LINEAR
                    },
                )),
                None => Slot::Missing,
            };
            slots.insert(exe, slot);
        }
    }
}

fn spawn(ctx: Context) -> Loader {
    let (req_tx, req_rx) = mpsc::channel::<Arc<str>>();
    let (res_tx, res_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("vigil-icons".into())
        .spawn(move || {
            platform::init_icon_thread();
            while let Ok(first) = req_rx.recv() {
                // Handle everything queued so far, then repaint once.
                for exe in std::iter::once(first).chain(req_rx.try_iter()) {
                    let image = platform::exe_icon(&exe)
                        .map(|(w, h, px)| ColorImage::from_rgba_unmultiplied([w, h], &px));
                    if res_tx.send((exe, image)).is_err() {
                        return;
                    }
                }
                ctx.request_repaint();
            }
        });
    Loader {
        requests: req_tx,
        results: res_rx,
    }
}

/// Draws the icon of `exe`, or a quiet placeholder while it loads or when there is none.
pub fn app_icon(ui: &mut egui::Ui, icons: &Icons, exe: &Arc<str>, size: f32, muted: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    if let Some(tex) = icons.get(ui.ctx(), exe) {
        let tint = if muted {
            Color32::from_white_alpha(210)
        } else {
            Color32::WHITE
        };
        egui::Image::new((tex.id(), rect.size()))
            .tint(tint)
            .paint_at(ui, rect);
        return;
    }
    let dark = ui.visuals().dark_mode;
    let fill = if dark {
        Color32::from_rgb(52, 54, 60)
    } else {
        Color32::from_rgb(226, 229, 234)
    };
    ui.painter()
        .rect_filled(rect.shrink(1.0), size * 0.22, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::APP_WINDOW,
        egui::FontId::proportional(size * 0.66),
        ui.visuals().weak_text_color(),
    );
}
