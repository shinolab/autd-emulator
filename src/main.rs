#[macro_use]
extern crate conrod_core;

mod autd_event_handler;
mod camera_helper;
mod color;
mod consts;
mod interface;
mod parser;
mod ui;
mod viewer_controller;

use std::sync::mpsc;

use acoustic_field_viewer::coloring_method::coloring_hsv;
use acoustic_field_viewer::view::event::*;
use acoustic_field_viewer::view::{
    AcousticFiledSliceViewer, SoundSourceViewer, UpdateHandler, ViewWindow, ViewerSettings,
};

use crate::autd_event_handler::AUTDEventHandler;
use crate::consts::TRANS_SIZE;
use crate::viewer_controller::ViewController;

fn main() {
    let mut interf = interface::Interface::open("127.0.0.1:50632").unwrap();

    let (tx_autd_event, rx_autd_event) = mpsc::channel();
    interf.start(tx_autd_event).unwrap();

    let mut settings = ViewerSettings::new(
        40e3,
        TRANS_SIZE,
        coloring_hsv,
        scarlet::colormap::ListedColorMap::inferno(),
    );
    settings.color_scale = 0.6;
    settings.slice_alpha = 0.95;
    let source_viewer = SoundSourceViewer::new();
    let mut acoustic_field_viewer = AcousticFiledSliceViewer::new();
    acoustic_field_viewer.translate([TRANS_SIZE * 8.5, TRANS_SIZE * 6.5, 150.]);

    let (from_ui, to_cnt) = mpsc::channel();
    let (from_cnt, to_ui) = mpsc::channel();

    let mut window = ViewWindow::new(vec![], source_viewer, acoustic_field_viewer, settings);

    let autd_event_handler = AUTDEventHandler::new(rx_autd_event);
    let mut viewer_controller = ViewController::new(to_cnt, from_cnt);
    let update = |update_handler: &mut UpdateHandler, button: Option<Button>| {
        autd_event_handler.update(update_handler);
        viewer_controller.update(update_handler, button);
    };

    let h = std::thread::spawn(move || ui::window_2d(to_ui, from_ui));

    window.update = Some(update);
    window.start();

    h.join().unwrap();

    interf.close();
}
