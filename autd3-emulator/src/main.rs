/*
 * File: main.rs
 * Project: src
 * Created Date: 06/07/2021
 * Author: Shun Suzuki
 * -----
 * Last Modified: 10/07/2021
 * Modified By: Shun Suzuki (suzuki@hapis.k.u-tokyo.ac.jp)
 * -----
 * Copyright (c) 2021 Hapis Lab. All rights reserved.
 *
 */

mod settings;

use std::{collections::VecDeque, f32::consts::PI, path::Path, time::Instant};

use acoustic_field_viewer::{
    camera_helper,
    sound_source::SoundSource,
    view::{
        render_system::RenderSystem, AcousticFiledSliceViewer, SoundSourceViewer, System,
        UpdateFlag,
    },
    Matrix4,
};
use autd3_core::hardware_defined::{
    RxGlobalControlFlags, MOD_SAMPLING_FREQ_BASE, POINT_SEQ_BASE_FREQ,
};
use autd3_emulator_server::{AutdData, AutdServer, DelayOffset, Modulation, Sequence};
use gfx::Device;
use glutin::{
    event::{Event, WindowEvent},
    event_loop::ControlFlow,
    platform::run_return::EventLoopExtRunReturn,
};
use imgui::*;
use shader_version::OpenGL;

use crate::settings::Setting;

struct App {
    setting: Setting,
    sources: Vec<SoundSource>,
    last_amp: Vec<f32>,
    sound_source_viewer: SoundSourceViewer,
    field_slice_viewer: AcousticFiledSliceViewer,
    view_projection: (Matrix4, Matrix4),
    init: bool,
    ctrl_flag: RxGlobalControlFlags,
    modulation: Option<Modulation>,
    sequence: Option<Sequence>,
    delay_offset: Option<DelayOffset>,
    log_buf: VecDeque<String>,
    #[cfg(feature = "offscreen_renderer")]
    offscreen_renderer: offscreen_renderer::OffscreenRenderer,
    save_path: ImString,
    record_path: ImString,
    recording: bool,
}

impl App {
    pub fn new(setting: Setting, system: &System) -> Self {
        let opengl = OpenGL::V4_5;
        let sound_source_viewer = SoundSourceViewer::new(&system.render_sys, opengl);
        let field_slice_viewer =
            AcousticFiledSliceViewer::new(&system.render_sys, opengl, &setting.viewer_setting);
        let view_projection = system
            .render_sys
            .get_view_projection(&setting.viewer_setting);

        let save_path = ImString::new(&setting.save_file_path);
        let record_path = ImString::new(&setting.record_path);
        Self {
            setting,
            sources: Vec::new(),
            last_amp: Vec::new(),
            sound_source_viewer,
            field_slice_viewer,
            view_projection,
            init: true,
            ctrl_flag: RxGlobalControlFlags::empty(),
            modulation: None,
            sequence: None,
            delay_offset: None,
            log_buf: VecDeque::new(),
            #[cfg(feature = "offscreen_renderer")]
            offscreen_renderer: offscreen_renderer::OffscreenRenderer::new(),
            save_path,
            record_path,
            recording: false,
        }
    }

    pub fn run(&mut self, system: System) {
        let System {
            mut events_loop,
            mut imgui,
            mut platform,
            mut render_sys,
            mut encoder,
            ..
        } = system;

        let mut autd_server = AutdServer::new(&format!("127.0.0.1:{}", self.setting.port)).unwrap();

        self.reset(&mut render_sys);

        let mut last_frame = Instant::now();
        let mut run = true;
        while run {
            events_loop.run_return(|event, _, control_flow| {
                self.handle_event(&mut render_sys, &event);
                platform.handle_event(imgui.io_mut(), render_sys.window(), &event);
                if let Event::WindowEvent { event, .. } = event {
                    match event {
                        WindowEvent::Resized(_) => render_sys.update_views(),
                        WindowEvent::CloseRequested => {
                            run = false;
                        }
                        _ => (),
                    }
                }
                *control_flow = ControlFlow::Exit;
            });
            if !run {
                break;
            }

            let io = imgui.io_mut();
            platform
                .prepare_frame(io, render_sys.window())
                .expect("Failed to start frame");
            let now = Instant::now();
            io.update_delta_time(now - last_frame);
            last_frame = now;
            let ui = imgui.frame();

            let mut update_flag = self.handle_autd(&mut autd_server);
            update_flag |= self.update_ui(&ui, &mut render_sys);
            self.update_view(&mut render_sys, update_flag);
            #[cfg(feature = "offscreen_renderer")]
            {
                if self.setting.save_file_enable {
                    self.offscreen_renderer.update(
                        &self.sources,
                        &self.field_slice_viewer,
                        &self.setting.viewer_setting,
                        update_flag,
                    );
                }
            }

            encoder.clear(
                &render_sys.output_color,
                self.setting.viewer_setting.background,
            );
            encoder.clear_depth(&render_sys.output_stencil, 1.0);
            self.sound_source_viewer.renderer(&mut encoder);
            self.field_slice_viewer.renderer(&mut encoder);

            platform.prepare_render(&ui, render_sys.window());
            let draw_data = ui.render();
            render_sys
                .renderer
                .render(
                    &mut render_sys.factory,
                    &mut encoder,
                    &mut render_sys.output_color,
                    draw_data,
                )
                .expect("Rendering failed");
            encoder.flush(&mut render_sys.device);
            render_sys.swap_buffers();
            render_sys.device.cleanup();
        }

        self.setting.save_file_path = self.save_path.to_str().to_owned();
        self.setting.record_path = self.record_path.to_str().to_owned();
        self.setting.merge_render_sys(&render_sys);
        self.setting.save("setting.json");
    }

    fn reset(&mut self, render_sys: &mut RenderSystem) {
        self.field_slice_viewer
            .move_to(self.setting.viewer_setting.slice_pos);
        self.field_slice_viewer
            .rotate_to(self.setting.viewer_setting.slice_angle);

        render_sys.camera.position = self.setting.viewer_setting.camera_pos;
        camera_helper::set_camera_angle(
            &mut render_sys.camera,
            self.setting.viewer_setting.camera_angle,
        );

        self.view_projection = render_sys.get_view_projection(&self.setting.viewer_setting);
    }

    fn handle_autd(&mut self, autd_server: &mut AutdServer) -> UpdateFlag {
        let mut update_flag = UpdateFlag::empty();
        autd_server.update(|data| {
            for d in data {
                match d {
                    AutdData::Geometries(geometries) => {
                        self.sources.clear();
                        for geometry in geometries {
                            for trans in geometry.make_autd_transducers() {
                                self.sources.push(trans);
                            }
                        }
                        self.log("geometry");
                        update_flag |= UpdateFlag::INIT_SOURCE;
                        update_flag |= UpdateFlag::UPDATE_SOURCE_DRIVE;
                    }
                    AutdData::Gain(gain) => {
                        for ((&phase, &amp), source) in gain
                            .phases
                            .iter()
                            .zip(gain.amps.iter())
                            .zip(self.sources.iter_mut())
                        {
                            source.amp = (amp as f32 / 510.0 * std::f32::consts::PI).sin();
                            source.phase = 2.0 * PI * (1.0 - (phase as f32 / 255.0));
                        }
                        self.log("gain");
                        update_flag |= UpdateFlag::UPDATE_SOURCE_DRIVE;
                    }
                    AutdData::Clear => {
                        for source in self.sources.iter_mut() {
                            source.amp = 0.;
                            source.phase = 0.;
                        }
                        self.modulation = None;
                        self.sequence = None;
                        self.delay_offset = None;
                        self.log("clear");
                        update_flag |= UpdateFlag::UPDATE_SOURCE_DRIVE;
                    }
                    AutdData::Pause => {
                        self.last_amp.clear();
                        for source in self.sources.iter_mut() {
                            self.last_amp.push(source.amp);
                            source.amp = 0.;
                        }
                        self.log("pause");
                        update_flag |= UpdateFlag::UPDATE_SOURCE_DRIVE;
                    }
                    AutdData::Resume => {
                        for (source, &amp) in self.sources.iter_mut().zip(self.last_amp.iter()) {
                            source.amp = amp;
                        }
                        self.last_amp.clear();
                        self.log("resume");
                        update_flag |= UpdateFlag::UPDATE_SOURCE_DRIVE;
                    }
                    AutdData::Modulation(m) => {
                        self.modulation = Some(m);
                        self.log("receive modulation");
                    }
                    AutdData::CtrlFlag(flag) => {
                        self.ctrl_flag = flag;
                    }
                    AutdData::RequestFpgaVerMsb => {
                        self.log("req fpga ver msb");
                    }
                    AutdData::RequestFpgaVerLsb => {
                        self.log("req fpga ver lsb");
                    }
                    AutdData::RequestCpuVerMsb => {
                        self.log("req cpu ver lsb");
                    }
                    AutdData::RequestCpuVerLsb => {
                        self.log("req cpu ver lsb");
                    }
                    AutdData::Sequence(seq) => {
                        self.sequence = Some(seq);
                        self.log("receive sequence");
                    }
                    AutdData::DelayOffset(d) => {
                        self.delay_offset = Some(d);
                        self.log("receive delay offset");
                    }
                }
            }
        });
        update_flag
    }

    fn handle_event(&mut self, render_sys: &mut RenderSystem, event: &Event<()>) {
        if self.init {
            self.update_view(render_sys, UpdateFlag::all());
            self.init = false;
        }
        self.sound_source_viewer.handle_event(&render_sys, event);
        self.field_slice_viewer.handle_event(&render_sys, event);
    }

    fn update_view(&mut self, render_sys: &mut RenderSystem, update_flag: UpdateFlag) {
        self.sound_source_viewer.update(
            render_sys,
            self.view_projection,
            &self.setting.viewer_setting,
            &self.sources,
            update_flag,
        );
        self.field_slice_viewer.update(
            render_sys,
            self.view_projection,
            &self.setting.viewer_setting,
            &self.sources,
            update_flag,
        );
    }

    fn update_ui(&mut self, ui: &Ui, render_sys: &mut RenderSystem) -> UpdateFlag {
        let mut update_flag = UpdateFlag::empty();
        Window::new(im_str!("Controller")).build(ui, || {
            TabBar::new(im_str!("Settings")).build(&ui, || {
                TabItem::new(im_str!("Slice")).build(&ui, || {
                    ui.text(im_str!("Slice size"));
                    if Slider::new(im_str!("Slice width"))
                        .range(0..=1000)
                        .build(&ui, &mut self.setting.viewer_setting.slice_width)
                    {
                        update_flag |= UpdateFlag::UPDATE_SLICE_SIZE;
                    }
                    if Slider::new(im_str!("Slice heigh"))
                        .range(0..=1000)
                        .build(&ui, &mut self.setting.viewer_setting.slice_height)
                    {
                        update_flag |= UpdateFlag::UPDATE_SLICE_SIZE;
                    }

                    ui.separator();
                    ui.text(im_str!("Slice position"));
                    if Drag::new(im_str!("Slice X"))
                        .build(&ui, &mut self.setting.viewer_setting.slice_pos[0])
                    {
                        self.field_slice_viewer
                            .move_to(self.setting.viewer_setting.slice_pos);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }
                    if Drag::new(im_str!("Slice Y"))
                        .build(&ui, &mut self.setting.viewer_setting.slice_pos[1])
                    {
                        self.field_slice_viewer
                            .move_to(self.setting.viewer_setting.slice_pos);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }
                    if Drag::new(im_str!("Slice Z"))
                        .build(&ui, &mut self.setting.viewer_setting.slice_pos[2])
                    {
                        self.field_slice_viewer
                            .move_to(self.setting.viewer_setting.slice_pos);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }

                    ui.separator();
                    ui.text(im_str!("Slice Rotation"));
                    if AngleSlider::new(im_str!("Slice RX"))
                        .range_degrees(0.0..=360.0)
                        .build(&ui, &mut self.setting.viewer_setting.slice_angle[0])
                    {
                        self.field_slice_viewer
                            .rotate_to(self.setting.viewer_setting.slice_angle);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }
                    if AngleSlider::new(im_str!("Slice RY"))
                        .range_degrees(0.0..=360.0)
                        .build(&ui, &mut self.setting.viewer_setting.slice_angle[1])
                    {
                        self.field_slice_viewer
                            .rotate_to(self.setting.viewer_setting.slice_angle);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }
                    if AngleSlider::new(im_str!("Slice RZ"))
                        .range_degrees(0.0..=360.0)
                        .build(&ui, &mut self.setting.viewer_setting.slice_angle[2])
                    {
                        self.field_slice_viewer
                            .rotate_to(self.setting.viewer_setting.slice_angle);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }

                    ui.separator();
                    ui.text(im_str!("Slice color setting"));
                    if Drag::new(im_str!("Color scale"))
                        .speed(0.1)
                        .range(0.0..=f32::INFINITY)
                        .build(&ui, &mut self.setting.viewer_setting.color_scale)
                    {
                        update_flag |= UpdateFlag::UPDATE_COLOR_MAP;
                    }
                    if Slider::new(im_str!("Slice alpha"))
                        .range(0.0..=1.0)
                        .build(&ui, &mut self.setting.viewer_setting.slice_alpha)
                    {
                        update_flag |= UpdateFlag::UPDATE_COLOR_MAP;
                    }

                    ui.separator();
                    if ui.small_button(im_str!("xy")) {
                        self.setting.viewer_setting.slice_angle = [0., 0., 0.];
                        self.field_slice_viewer
                            .rotate_to(self.setting.viewer_setting.slice_angle);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }
                    ui.same_line(0.);
                    if ui.small_button(im_str!("yz")) {
                        self.setting.viewer_setting.slice_angle = [0., -PI / 2., 0.];
                        self.field_slice_viewer
                            .rotate_to(self.setting.viewer_setting.slice_angle);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }
                    ui.same_line(0.);
                    if ui.small_button(im_str!("zx")) {
                        self.setting.viewer_setting.slice_angle = [PI / 2., 0., 0.];
                        self.field_slice_viewer
                            .rotate_to(self.setting.viewer_setting.slice_angle);
                        update_flag |= UpdateFlag::UPDATE_SLICE_POS;
                    }

                    #[cfg(feature = "offscreen_renderer")]
                    {
                        ui.separator();
                        ui.text(im_str!("Save as file"));
                        if ui.radio_button_bool(
                            im_str!("save enable"),
                            self.setting.save_file_enable,
                        ) {
                            self.setting.save_file_enable = !self.setting.save_file_enable;
                        }
                        if self.setting.save_file_enable {
                            InputText::new(ui, im_str!("save path"), &mut self.save_path).build();
                            if ui.small_button(im_str!("save")) {
                                self.offscreen_renderer
                                    .calculate_field(&self.sources, &self.setting.viewer_setting);
                                let bb = (
                                    self.setting.viewer_setting.slice_width as usize,
                                    self.setting.viewer_setting.slice_height as usize,
                                );
                                self.offscreen_renderer.save(
                                    self.save_path.to_str(),
                                    bb,
                                    self.field_slice_viewer.color_map(),
                                );
                            }

                            ui.separator();
                            InputText::new(ui, im_str!("record path"), &mut self.record_path)
                                .build();
                            if ui.small_button(if self.recording {
                                im_str!("stop recording")
                            } else {
                                im_str!("record")
                            }) {
                                self.recording = !self.recording;
                            }
                            if self.recording {
                                self.offscreen_renderer
                                    .calculate_field(&self.sources, &self.setting.viewer_setting);
                                let bb = (
                                    self.setting.viewer_setting.slice_width as usize,
                                    self.setting.viewer_setting.slice_height as usize,
                                );
                                std::fs::create_dir_all(self.record_path.to_str()).unwrap();
                                let date = chrono::Local::now();
                                let path = Path::new(self.record_path.to_str())
                                    .join(format!("{}", date.format("%Y-%m-%d_%H-%M-%S_%3f.png")));
                                self.offscreen_renderer.save(
                                    &path,
                                    bb,
                                    self.field_slice_viewer.color_map(),
                                );
                            }
                        }
                    }
                });
                TabItem::new(im_str!("Camera")).build(&ui, || {
                    ui.text(im_str!("Camera pos"));
                    if Drag::new(im_str!("Camera X"))
                        .build(&ui, &mut self.setting.viewer_setting.camera_pos[0])
                    {
                        render_sys.camera.position = self.setting.viewer_setting.camera_pos;
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }
                    if Drag::new(im_str!("Camera Y"))
                        .build(&ui, &mut self.setting.viewer_setting.camera_pos[1])
                    {
                        render_sys.camera.position = self.setting.viewer_setting.camera_pos;
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }
                    if Drag::new(im_str!("Camera Z"))
                        .build(&ui, &mut self.setting.viewer_setting.camera_pos[2])
                    {
                        render_sys.camera.position = self.setting.viewer_setting.camera_pos;
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }

                    ui.separator();
                    ui.text(im_str!("Camera rotation"));
                    if AngleSlider::new(im_str!("Camera RX"))
                        .range_degrees(-180.0..=180.0)
                        .build(&ui, &mut self.setting.viewer_setting.camera_angle[0])
                    {
                        camera_helper::set_camera_angle(
                            &mut render_sys.camera,
                            self.setting.viewer_setting.camera_angle,
                        );
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }
                    if AngleSlider::new(im_str!("Camera RY"))
                        .range_degrees(-180.0..=180.0)
                        .build(&ui, &mut self.setting.viewer_setting.camera_angle[1])
                    {
                        camera_helper::set_camera_angle(
                            &mut render_sys.camera,
                            self.setting.viewer_setting.camera_angle,
                        );
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }
                    if AngleSlider::new(im_str!("Camera RZ"))
                        .range_degrees(-180.0..=180.0)
                        .build(&ui, &mut self.setting.viewer_setting.camera_angle[2])
                    {
                        camera_helper::set_camera_angle(
                            &mut render_sys.camera,
                            self.setting.viewer_setting.camera_angle,
                        );
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }

                    ui.separator();
                    ui.text(im_str!("Camera perspective"));
                    if AngleSlider::new(im_str!("FOV"))
                        .range_degrees(0.0..=180.0)
                        .build(&ui, &mut self.setting.viewer_setting.fov)
                    {
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }
                    if Drag::new(im_str!("Near clip"))
                        .range(0.0..=f32::INFINITY)
                        .build(&ui, &mut self.setting.viewer_setting.near_clip)
                    {
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }
                    if Drag::new(im_str!("Far clip"))
                        .range(0.0..=f32::INFINITY)
                        .build(&ui, &mut self.setting.viewer_setting.far_clip)
                    {
                        self.view_projection =
                            render_sys.get_view_projection(&self.setting.viewer_setting);
                        update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
                    }
                });
                TabItem::new(im_str!("Config")).build(&ui, || {
                    if Drag::new(im_str!("Wavelength"))
                        .speed(0.1)
                        .range(0.0..=f32::INFINITY)
                        .build(&ui, &mut self.setting.viewer_setting.wave_length)
                    {
                        update_flag |= UpdateFlag::UPDATE_WAVENUM;
                    }
                    ui.separator();
                    if Slider::new(im_str!("Transducer alpha"))
                        .range(0.0..=1.0)
                        .build(&ui, &mut self.setting.viewer_setting.source_alpha)
                    {
                        update_flag |= UpdateFlag::UPDATE_SOURCE_ALPHA;
                    }
                    ui.separator();
                    ColorPicker::new(
                        im_str!("Background"),
                        &mut self.setting.viewer_setting.background,
                    )
                    .alpha(true)
                    .build(&ui);
                });
                TabItem::new(im_str!("Info")).build(&ui, || {
                    ui.text("Control flag");
                    let mut flag = self.ctrl_flag;
                    ui.checkbox_flags(
                        im_str!("MOD BEGIN"),
                        &mut flag,
                        RxGlobalControlFlags::MOD_BEGIN,
                    );
                    ui.checkbox_flags(im_str!("MOD END"), &mut flag, RxGlobalControlFlags::MOD_END);
                    ui.checkbox_flags(
                        im_str!("MOD END"),
                        &mut flag,
                        RxGlobalControlFlags::READ_FPGA_INFO,
                    );
                    ui.checkbox_flags(im_str!("SILENT"), &mut flag, RxGlobalControlFlags::SILENT);
                    ui.checkbox_flags(
                        im_str!("FORCE FAN"),
                        &mut flag,
                        RxGlobalControlFlags::FORCE_FAN,
                    );
                    ui.checkbox_flags(
                        im_str!("SEQ MODE"),
                        &mut flag,
                        RxGlobalControlFlags::SEQ_MODE,
                    );
                    ui.checkbox_flags(
                        im_str!("SEQ BEGIN"),
                        &mut flag,
                        RxGlobalControlFlags::SEQ_BEGIN,
                    );
                    ui.checkbox_flags(im_str!("SEQ END"), &mut flag, RxGlobalControlFlags::SEQ_END);

                    if let Some(m) = &self.modulation {
                        ui.separator();
                        ui.text("Modulation");
                        ui.text(format!("Modulation size: {}", m.mod_data.len()));
                        ui.text(format!("Modulation division: {}", m.mod_div));
                        let smpl_period =
                            (1000000.0 / MOD_SAMPLING_FREQ_BASE) as usize * m.mod_div as usize;
                        ui.text(format!("Modulation sampling period: {} [us]", smpl_period));
                        ui.text(format!(
                            "Modulation period: {} [us]",
                            smpl_period * m.mod_data.len()
                        ));
                        if !m.mod_data.is_empty() {
                            ui.text(format!("mod[0]: {}", m.mod_data[0]));
                        }
                        if m.mod_data.len() == 2 || m.mod_data.len() == 3 {
                            ui.text(format!("mod[1]: {}", m.mod_data[1]));
                        } else if m.mod_data.len() > 3 {
                            ui.text("...");
                        }
                        if m.mod_data.len() >= 3 {
                            let idx = m.mod_data.len() - 1;
                            ui.text(format!("mod[{}]: {}", idx, m.mod_data[idx]));
                        }

                        if ui
                            .radio_button_bool(im_str!("show mod plot"), self.setting.show_mod_plot)
                        {
                            self.setting.show_mod_plot = !self.setting.show_mod_plot;
                        }

                        if self.setting.show_mod_plot {
                            ui.separator();
                            let mod_v = self.mod_values(|&v| ((v as f32) / 512.0 * PI).sin());
                            PlotLines::new(ui, im_str!("mod plot"), &mod_v)
                                .graph_size(self.setting.mod_plot_size)
                                .build();
                            if ui.radio_button_bool(
                                im_str!("show mod plot (raw)"),
                                self.setting.show_mod_plot_raw,
                            ) {
                                self.setting.show_mod_plot_raw = !self.setting.show_mod_plot_raw;
                            }
                            if self.setting.show_mod_plot_raw {
                                ui.separator();
                                let mod_v = self.mod_values(|&v| v as f32);
                                PlotLines::new(ui, im_str!("mod plot (raw)"), &mod_v)
                                    .graph_size(self.setting.mod_plot_size)
                                    .build();
                            }

                            Drag::new(im_str!("plot size"))
                                .range(0.0..=f32::INFINITY)
                                .build_array(ui, &mut self.setting.mod_plot_size);
                        }
                    }

                    if self.ctrl_flag.contains(RxGlobalControlFlags::SEQ_MODE) {
                        ui.separator();
                        ui.text("Sequence mode");
                        if let Some(seq) = &self.sequence {
                            ui.text(format!("Sequence size: {}", seq.seq_data.len()));
                            ui.text(format!("Sequence division: {}", seq.seq_div));
                            let smpl_period =
                                (1000000 / POINT_SEQ_BASE_FREQ) * seq.seq_div as usize;
                            ui.text(format!("Sequence sampling period: {} [us]", smpl_period));
                            ui.text(format!(
                                "Sequence period: {} [us]",
                                smpl_period * seq.seq_data.len()
                            ));
                            if !seq.seq_data.is_empty() {
                                ui.text(format!(
                                    "seq[0]: {:?} / {}",
                                    seq.seq_data[0].0, seq.seq_data[0].1
                                ));
                            }
                            if seq.seq_data.len() == 2 || seq.seq_data.len() == 3 {
                                ui.text(format!(
                                    "seq[1]: {:?} / {}",
                                    seq.seq_data[1].0, seq.seq_data[1].1
                                ));
                            } else if seq.seq_data.len() > 3 {
                                ui.text("...");
                            }
                            if seq.seq_data.len() >= 3 {
                                let idx = seq.seq_data.len() - 1;
                                ui.text(format!(
                                    "seq[{}]: {:?} / {}",
                                    idx, seq.seq_data[idx].0, seq.seq_data[idx].1
                                ));
                            }
                        }
                    }

                    if let Some(d) = &self.delay_offset {
                        ui.separator();
                        ui.text("Duty offset and Delay");
                        ui.text(format!(
                            "offset[0]: {}, delay[0]: {}",
                            d.delay_offset[0].1, d.delay_offset[0].0
                        ));
                        ui.text("...");
                        let idx = d.delay_offset.len() - 1;
                        ui.text(format!(
                            "offset[{0}]: {1}, delay[{0}]: {2}",
                            idx, d.delay_offset[idx].1, d.delay_offset[idx].0
                        ));
                    }
                });
                TabItem::new(im_str!("Log")).build(&ui, || {
                    if ui.radio_button_bool(im_str!("enable"), self.setting.log_enable) {
                        self.setting.log_enable = !self.setting.log_enable;
                    }
                    if self.setting.log_enable {
                        Slider::new(im_str!("Max"))
                            .range(0..=1000)
                            .build(&ui, &mut self.setting.log_max);

                        ui.text(self.get_log_txt());
                    }
                });
            });

            ui.separator();

            if ui.small_button(im_str!("auto")) {
                let rot = quaternion::euler_angles(
                    self.setting.viewer_setting.slice_angle[0],
                    self.setting.viewer_setting.slice_angle[1],
                    self.setting.viewer_setting.slice_angle[2],
                );
                let model = vecmath_util::mat4_rot(rot);

                let right = vecmath_util::to_vec3(&model[0]);
                let up = vecmath_util::to_vec3(&model[1]);
                let forward = vecmath::vec3_cross(right, up);

                let d = vecmath::vec3_scale(forward, 500.);
                let p = vecmath::vec3_add(
                    vecmath_util::to_vec3(&self.setting.viewer_setting.slice_pos),
                    d,
                );

                self.setting.viewer_setting.camera_pos = p;
                render_sys.camera.position = p;
                render_sys.camera.right = right;
                render_sys.camera.up = up;
                render_sys.camera.look_at(vecmath_util::to_vec3(
                    &self.setting.viewer_setting.slice_pos,
                ));
                self.setting.viewer_setting.camera_angle =
                    camera_helper::rot_mat_to_euler_angles(&[
                        render_sys.camera.right,
                        render_sys.camera.up,
                        render_sys.camera.forward,
                    ]);
                camera_helper::set_camera_angle(
                    &mut render_sys.camera,
                    self.setting.viewer_setting.camera_angle,
                );
                self.view_projection = render_sys.get_view_projection(&self.setting.viewer_setting);
                update_flag |= UpdateFlag::UPDATE_CAMERA_POS;
            }

            ui.same_line(0.);
            if ui.small_button(im_str!("reset")) {
                self.setting = Setting::load("setting.json");
                self.reset(render_sys);
                update_flag = UpdateFlag::all();
            }

            ui.same_line(0.);
            if ui.small_button(im_str!("default")) {
                let default_setting = acoustic_field_viewer::view::ViewerSettings {
                    wave_length: self.setting.viewer_setting.wave_length,
                    ..Default::default()
                };
                self.setting.viewer_setting = default_setting;
                self.reset(render_sys);
                update_flag = UpdateFlag::all();
            }
        });

        update_flag
    }

    fn mod_values<F>(&self, f: F) -> Vec<f32>
    where
        F: Fn(&u8) -> f32,
    {
        if let Some(m) = &self.modulation {
            m.mod_data.iter().map(f).collect()
        } else {
            vec![]
        }
    }

    // TODO: This log system is not so efficient
    fn log(&mut self, msg: &str) {
        if self.setting.log_enable {
            let date = chrono::Local::now();
            self.log_buf
                .push_back(format!("{}: {}", date.format("%Y-%m-%d %H:%M:%S.%3f"), msg));
            while self.log_buf.len() > self.setting.log_max as usize {
                self.log_buf.pop_front();
            }
        }
    }

    fn get_log_txt(&self) -> String {
        let mut log = String::new();
        for line in &self.log_buf {
            log.push_str(line);
            log.push('\n');
        }
        log
    }
}

pub fn main() {
    let setting = Setting::load("setting.json");
    let system = System::init(
        "AUTD3 emulator",
        setting.window_width as _,
        setting.window_height as _,
    );

    let mut app = App::new(setting, &system);
    app.run(system);
}
