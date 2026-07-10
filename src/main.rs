use core::f64;
use std::{cell::Cell, rc::Rc};

use gtk4::{ApplicationWindow, Button, Label, Orientation, prelude::*};
use gtk4_layer_shell::{Edge, Layer, LayerShell};
use volumecontrol::AudioDevice;

const APP_ID: &str = "top.hxdbk.gtk-layer-sound";
const WINDOW_WIDTH: i32 = 500;
const WINDOW_HEIGHT: i32 = 200;
const BIAS: i32 = 1300;

fn activate(app: &gtk4::Application) -> Result<(), volumecontrol::AudioError> {
    install_css();
    LocalWindowBuilder::new(app)?
        .set_windows()
        .set_box()
        .build()
        .show();
    Ok(())
}

fn install_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(include_str!("resources/style.css"));

    let display = gtk4::gdk::Display::default().expect("GTK display is unavailable");
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn main() -> Result<(), volumecontrol::AudioError> {
    let application = gtk4::Application::new(Some(APP_ID), Default::default());

    application.connect_activate(|app| {
        if let Err(error) = activate(app) {
            eprintln!("failed to initialize the audio controls: {error}");
        }
    });

    application.run();
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum Message {
    SIncrement,
    SDecrement,
    MIncrement,
    MDecrement,
}

#[derive(Debug)]
struct LocalWindowBuilder {
    w: ApplicationWindow,
    speaker_volume: Rc<Cell<f64>>,
    microphone_volume: Rc<Cell<f64>>,
}

impl LocalWindowBuilder {
    fn new(app: &gtk4::Application) -> Result<Self, volumecontrol::AudioError> {
        let device = AudioDevice::from_default()?;
        let current_vol = device.get_vol()?;
        Ok(Self {
            w: ApplicationWindow::builder().application(app).build(),
            speaker_volume: Rc::new(Cell::new(f64::from(current_vol))),
            microphone_volume: Rc::new(Cell::new(0.0)),
        })
    }

    fn set_windows(self) -> Self {
        self.w.add_css_class("sound-window");
        self.w.init_layer_shell();
        self.w.set_default_size(WINDOW_WIDTH, WINDOW_HEIGHT);
        //放在最顶层
        self.w.set_layer(Layer::Overlay);

        for (anchor, state) in [
            (Edge::Left, true),
            (Edge::Right, false),
            (Edge::Top, true),
            (Edge::Bottom, false),
        ] {
            self.w.set_anchor(anchor, state);
        }

        self.w.set_margin(Edge::Left, BIAS);
        self
    }

    fn set_box(self) -> Self {
        let content = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .build();
        content.add_css_class("sound-panel");
        let speaker_box = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();

        let microphone_box = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        let s_button_up = Button::builder().label("+").build();
        s_button_up.add_css_class("volume-button");
        let speaker_volume = Rc::clone(&self.speaker_volume);
        let speaker_label = Label::new(Some(&speaker_volume.get().to_string()));
        let label = speaker_label.clone();
        s_button_up.connect_clicked(move |_| {
            let value = Self::adjust_volume(&speaker_volume, Message::SIncrement, 1.0)
                .expect("failed to increase speaker volume");
            label.set_text(&value.to_string());
        });
        let s_button_down = Button::builder().label("-").build();
        s_button_down.add_css_class("volume-button");
        let speaker_volume = Rc::clone(&self.speaker_volume);
        let label = speaker_label.clone();
        s_button_down.connect_clicked(move |_| {
            let value = Self::adjust_volume(&speaker_volume, Message::SDecrement, 1.0)
                .expect("failed to decrease speaker volume");
            label.set_text(&value.to_string());
        });
        // 核心步骤：使用 Adjustment 定义范围 (值, 最小, 最大, 步进...)
        let speaker_adjustment =
            gtk4::Adjustment::new(self.speaker_volume.get(), 0.0, 100.0, 1.0, 10.0, 0.0);

        // 创建 Horizontal(水平) 或 Vertical(垂直) 滑动条
        let speaker_scale = gtk4::Scale::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .adjustment(&speaker_adjustment)
            .draw_value(true) // 显示数值
            .digits(0) // 小数位数
            .width_request(180)
            .hexpand(true)
            .build();

        // 信号处理：监听数值变化
        let speaker_volume = Rc::clone(&self.speaker_volume);
        let label = speaker_label.clone();
        speaker_scale.connect_value_changed(move |s| {
            let value =
                Self::set_volume(&speaker_volume, s.value()).expect("failed to set speaker volume");
            label.set_text(&value.to_string());
        });

        let m_button_up = Button::builder().label("+").build();
        m_button_up.add_css_class("volume-button");
        let microphone_volume = Rc::clone(&self.microphone_volume);
        let microphone_label = Label::new(Some(&microphone_volume.get().to_string()));
        let label = microphone_label.clone();
        m_button_up.connect_clicked(move |_| {
            let value = Self::adjust_volume(&microphone_volume, Message::MIncrement, 1.0)
                .expect("failed to increase microphone volume");
            label.set_text(&value.to_string());
        });
        let m_button_down = Button::builder().label("-").build();
        m_button_down.add_css_class("volume-button");
        let microphone_volume = Rc::clone(&self.microphone_volume);
        let label = microphone_label.clone();
        m_button_down.connect_clicked(move |_| {
            let value = Self::adjust_volume(&microphone_volume, Message::MDecrement, 1.0)
                .expect("failed to decrease microphone volume");
            label.set_text(&value.to_string());
        });
        speaker_box.append(&s_button_up);
        speaker_box.append(&s_button_down);
        speaker_box.append(&speaker_label);
        speaker_box.append(&speaker_scale);
        microphone_box.append(&m_button_up);
        microphone_box.append(&m_button_down);
        microphone_box.append(&microphone_label);
        content.append(&speaker_box);
        content.append(&microphone_box);
        self.w.set_child(Some(&content));

        self
    }

    fn build(self) -> ApplicationWindow {
        self.w
    }

    fn adjust_volume(
        volume: &Cell<f64>,
        message: Message,
        step: f64,
    ) -> Result<f64, volumecontrol::AudioError> {
        let value = match message {
            Message::SIncrement | Message::MIncrement => volume.get() + step,
            Message::SDecrement | Message::MDecrement => volume.get() - step,
        };
        match message {
            Message::SIncrement | Message::SDecrement => Self::set_volume(volume, value),
            Message::MIncrement | Message::MDecrement => {
                let value = value.clamp(0.0, 100.0);
                volume.set(value);
                Ok(value)
            }
        }
    }

    fn set_volume(volume: &Cell<f64>, value: f64) -> Result<f64, volumecontrol::AudioError> {
        let value = value.clamp(0.0, 100.0);
        volume.set(value);
        let device = AudioDevice::from_default()?;
        device.set_vol(value.round() as u8)?;

        Ok(value)
    }
}
