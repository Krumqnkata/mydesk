#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(deprecated)]
//! MyDesk — a fast, encrypted remote desktop for Windows and Linux.
//!
//! Usage:
//!   mydesk                      open the app
//!   mydesk --relay [port]       run a relay server (default port 47900), no window
//!   mydesk --selftest           check the whole pipeline over loopback (view only)
//!   mydesk --screenshot <png>   open, draw ~120 frames, save a picture of the window, exit

mod app;
mod capture;
mod client;
mod codec;
mod config;
mod extras;
mod host;
mod input;
mod net;
mod proto;
mod selftest;
mod theme;
mod view;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--selftest") {
        std::process::exit(selftest::run());
    }

    if let Some(i) = args.iter().position(|a| a == "--relay") {
        let port = args.get(i + 1).and_then(|p| p.parse().ok()).unwrap_or(proto::RELAY_PORT);
        if let Err(e) = net::run_relay_server(port) {
            eprintln!("relay failed: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MyDesk")
        .with_app_id("mydesk")
        .with_inner_size([1240.0, 780.0])
        .with_min_inner_size([960.0, 620.0])
        .with_icon(theme::window_icon())
        .with_drag_and_drop(true);
    if screenshot.is_some() {
        viewport = viewport.with_active(false);
        #[cfg(windows)]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }

    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions { viewport, ..Default::default() };
    #[cfg(target_os = "macos")]
    if screenshot.is_some() {
        options.event_loop_builder = Some(Box::new(|builder| {
            use eframe::winit::platform::macos::EventLoopBuilderExtMacOS;
            builder.with_activate_ignoring_other_apps(false);
        }));
    }

    eframe::run_native("MyDesk", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, screenshot)))))
}
