mod audio;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use audio::{AudioCommand, AudioDevice, AudioEvent, AudioService, EndpointKind, EndpointState};
use gtk4::{
    ApplicationWindow, Button, DropDown, Image, Label, Orientation, ProgressBar, Scale, StringList,
    Switch, prelude::*,
};
use gtk4_layer_shell::{Edge, Layer, LayerShell};

const APP_ID: &str = "top.hxdbk.gtk-layer-sound";
const WINDOW_WIDTH: i32 = 520;
const WINDOW_HEIGHT: i32 = 420;
const BIAS: i32 = 1300;

fn main() {
    let application = gtk4::Application::new(Some(APP_ID), Default::default());
    application.connect_activate(activate);
    application.run();
}

fn activate(app: &gtk4::Application) {
    install_css();
    let service = AudioService::spawn();
    let window = ApplicationWindow::builder()
        .application(app)
        .default_width(WINDOW_WIDTH)
        .default_height(WINDOW_HEIGHT)
        .build();
    configure_layer_shell(&window);
    let ui = Rc::new(Controls::new());
    bind_controls(&ui, service.commands.clone());
    window.set_child(Some(&ui.root));
    poll_events(Rc::clone(&ui), service);
    window.present();
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

fn configure_layer_shell(window: &ApplicationWindow) {
    window.add_css_class("sound-window");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    for (edge, active) in [
        (Edge::Left, true),
        (Edge::Right, false),
        (Edge::Top, true),
        (Edge::Bottom, false),
    ] {
        window.set_anchor(edge, active);
    }
    window.set_margin(Edge::Left, BIAS);
}

struct EndpointWidgets {
    kind: EndpointKind,
    card: gtk4::Box,
    devices: StringList,
    selector: DropDown,
    volume: Scale,
    mute: Switch,
    value: Label,
    selected: Rc<RefCell<Vec<AudioDevice>>>,
    updating: Rc<Cell<bool>>,
}

struct Controls {
    root: gtk4::Box,
    output: EndpointWidgets,
    input: EndpointWidgets,
    level: ProgressBar,
    meter_state: Label,
    error: Label,
    error_revealer: gtk4::Revealer,
    refresh: Button,
}

impl Controls {
    fn new() -> Self {
        let root = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(12)
            .margin_top(14)
            .margin_bottom(14)
            .margin_start(14)
            .margin_end(14)
            .build();
        root.add_css_class("sound-panel");

        let header = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(12)
            .build();
        header.add_css_class("panel-header");
        let brand_icon = gtk4::Box::builder()
            .halign(gtk4::Align::Center)
            .valign(gtk4::Align::Center)
            .build();
        brand_icon.add_css_class("brand-icon");
        let brand_glyph = Image::from_icon_name("audio-volume-high-symbolic");
        brand_glyph.set_pixel_size(22);
        brand_glyph.set_hexpand(true);
        brand_glyph.set_vexpand(true);
        brand_glyph.set_halign(gtk4::Align::Center);
        brand_glyph.set_valign(gtk4::Align::Center);
        brand_icon.append(&brand_glyph);
        let heading = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(1)
            .hexpand(true)
            .build();
        let eyebrow = Label::new(Some("SYSTEM AUDIO"));
        eyebrow.set_xalign(0.0);
        eyebrow.add_css_class("panel-eyebrow");
        let title = Label::new(Some("声音控制"));
        title.set_xalign(0.0);
        title.add_css_class("panel-title");
        let description = Label::new(Some("管理当前会话的播放与录音设备"));
        description.set_xalign(0.0);
        description.add_css_class("panel-description");
        heading.append(&eyebrow);
        heading.append(&title);
        heading.append(&description);
        let refresh = Button::builder().label("刷新设备").build();
        refresh.add_css_class("refresh-button");
        refresh.set_tooltip_text(Some("重新扫描音频设备"));
        header.append(&brand_icon);
        header.append(&heading);
        header.append(&refresh);
        root.append(&header);

        let output = endpoint_widgets(
            EndpointKind::Output,
            "输出设备",
            "耳机、扬声器和其他播放设备",
        );
        let input = endpoint_widgets(EndpointKind::Input, "麦克风", "输入电平与录音设备");
        let meter = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(10)
            .build();
        meter.add_css_class("meter-row");
        let meter_label = Label::new(Some("输入电平"));
        meter_label.add_css_class("meter-label");
        let level = ProgressBar::new();
        level.set_hexpand(true);
        level.add_css_class("input-level");
        let meter_state = Label::new(Some("等待信号"));
        meter_state.add_css_class("meter-state");
        meter.append(&meter_label);
        meter.append(&level);
        meter.append(&meter_state);
        input.card.append(&meter);
        root.append(&output.card);
        root.append(&input.card);
        let error = Label::new(None);
        error.set_wrap(true);
        error.set_xalign(0.0);
        error.add_css_class("error-text");
        let error_revealer = gtk4::Revealer::builder()
            .transition_type(gtk4::RevealerTransitionType::SlideDown)
            .transition_duration(180)
            .child(&error)
            .build();
        root.append(&error_revealer);
        Self {
            root,
            output,
            input,
            level,
            meter_state,
            error,
            error_revealer,
            refresh,
        }
    }
    fn endpoint(&self, kind: EndpointKind) -> &EndpointWidgets {
        if kind == EndpointKind::Output {
            &self.output
        } else {
            &self.input
        }
    }
    fn apply_snapshot(&self, state: EndpointState) {
        let endpoint = self.endpoint(state.kind);
        endpoint.updating.set(true);
        endpoint.devices.splice(0, endpoint.devices.n_items(), &[]);
        for device in &state.devices {
            endpoint.devices.append(&device.label);
        }
        let selected = state
            .default_name
            .as_deref()
            .and_then(|name| state.devices.iter().position(|device| device.name == name))
            .unwrap_or(0);
        endpoint.selected.replace(state.devices);
        endpoint.selector.set_selected(selected as u32);
        if let Some(device) = endpoint.selected.borrow().get(selected) {
            endpoint.volume.set_value(device.volume * 100.0);
            endpoint.mute.set_active(device.muted);
            endpoint
                .value
                .set_text(&format!("{:.0}%", device.volume * 100.0));
            endpoint.card.set_sensitive(true);
            endpoint.card.remove_css_class("muted");
            if device.muted {
                endpoint.card.add_css_class("muted");
            }
        } else {
            endpoint.card.set_sensitive(false);
        }
        endpoint.updating.set(false);
    }
}

fn endpoint_widgets(kind: EndpointKind, heading: &str, subtitle: &str) -> EndpointWidgets {
    let card = gtk4::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    card.add_css_class("endpoint-card");
    let head = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(10)
        .build();
    let icon = gtk4::Box::builder()
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .build();
    icon.add_css_class("endpoint-icon");
    let icon_name = match kind {
        EndpointKind::Output => "audio-speakers-symbolic",
        EndpointKind::Input => "audio-input-microphone-symbolic",
    };
    let glyph = Image::from_icon_name(icon_name);
    glyph.set_pixel_size(18);
    glyph.set_hexpand(true);
    glyph.set_vexpand(true);
    glyph.set_halign(gtk4::Align::Center);
    glyph.set_valign(gtk4::Align::Center);
    icon.append(&glyph);
    let labels = gtk4::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(1)
        .hexpand(true)
        .build();
    let title = Label::new(Some(heading));
    title.set_xalign(0.0);
    title.add_css_class("endpoint-title");
    let detail = Label::new(Some(subtitle));
    detail.set_xalign(0.0);
    detail.add_css_class("endpoint-subtitle");
    labels.append(&title);
    labels.append(&detail);
    let mute_group = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(7)
        .valign(gtk4::Align::Center)
        .build();
    mute_group.add_css_class("mute-group");
    let mute_label = Label::new(Some("静音"));
    mute_label.add_css_class("control-label");
    let mute = Switch::new();
    mute.add_css_class("mute-switch");
    mute.set_tooltip_text(Some("切换设备静音"));
    mute_group.append(&mute_label);
    mute_group.append(&mute);
    head.append(&icon);
    head.append(&labels);
    head.append(&mute_group);
    let devices = StringList::new(&[]);
    let selector = DropDown::builder().model(&devices).hexpand(true).build();
    selector.add_css_class("device-selector");
    selector.set_tooltip_text(Some("选择当前使用的音频设备"));
    let selector_row = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(10)
        .build();
    let selector_label = Label::new(Some("设备"));
    selector_label.set_width_chars(4);
    selector_label.set_xalign(0.0);
    selector_label.add_css_class("control-label");
    selector_row.append(&selector_label);
    selector_row.append(&selector);
    let controls = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(10)
        .build();
    let volume_label = Label::new(Some("音量"));
    volume_label.set_width_chars(4);
    volume_label.set_xalign(0.0);
    volume_label.add_css_class("control-label");
    let volume = Scale::with_range(Orientation::Horizontal, 0.0, 150.0, 1.0);
    volume.set_draw_value(false);
    volume.set_hexpand(true);
    volume.add_css_class("volume-scale");
    volume.set_tooltip_text(Some("调节设备音量，最高 150%"));
    let value = Label::new(Some("0%"));
    value.set_width_chars(4);
    value.add_css_class("volume-value");
    controls.append(&volume_label);
    controls.append(&volume);
    controls.append(&value);
    card.append(&head);
    card.append(&selector_row);
    card.append(&controls);
    EndpointWidgets {
        kind,
        card,
        devices,
        selector,
        volume,
        mute,
        value,
        selected: Rc::new(RefCell::new(Vec::new())),
        updating: Rc::new(Cell::new(false)),
    }
}

fn bind_controls(ui: &Rc<Controls>, commands: std::sync::mpsc::Sender<AudioCommand>) {
    let refresh = commands.clone();
    ui.refresh.connect_clicked(move |_| {
        let _ = refresh.send(AudioCommand::Refresh);
    });
    bind_endpoint(&ui.output, commands.clone());
    bind_endpoint(&ui.input, commands);
}

fn bind_endpoint(endpoint: &EndpointWidgets, commands: std::sync::mpsc::Sender<AudioCommand>) {
    let kind = endpoint.kind;
    let devices = Rc::clone(&endpoint.selected);
    let updating = Rc::clone(&endpoint.updating);
    let tx = commands.clone();
    endpoint.selector.connect_selected_notify(move |selector| {
        if updating.get() {
            return;
        }
        if let Some(device) = devices.borrow().get(selector.selected() as usize) {
            let _ = tx.send(AudioCommand::Select {
                kind,
                index: device.index,
            });
        }
    });
    let kind = endpoint.kind;
    let devices = Rc::clone(&endpoint.selected);
    let updating = Rc::clone(&endpoint.updating);
    let selector = endpoint.selector.clone();
    let tx = commands.clone();
    let pending_volume = Rc::new(RefCell::new(None::<gtk4::glib::SourceId>));
    let latest_volume = Rc::new(Cell::new(0.0));
    endpoint.volume.connect_value_changed(move |scale| {
        if updating.get() {
            return;
        }
        latest_volume.set(scale.value() / 100.0);
        if pending_volume.borrow().is_none() {
            let pending = Rc::clone(&pending_volume);
            let latest = Rc::clone(&latest_volume);
            let devices = Rc::clone(&devices);
            let selector = selector.clone();
            let tx = tx.clone();
            let source = gtk4::glib::timeout_add_local_once(Duration::from_millis(80), move || {
                pending.borrow_mut().take();
                if let Some(device) = devices.borrow().get(selector.selected() as usize) {
                    let _ = tx.send(AudioCommand::SetVolume {
                        kind,
                        index: device.index,
                        volume: latest.get(),
                    });
                }
            });
            pending_volume.replace(Some(source));
        }
    });
    let kind = endpoint.kind;
    let devices = Rc::clone(&endpoint.selected);
    let updating = Rc::clone(&endpoint.updating);
    let selector = endpoint.selector.clone();
    endpoint.mute.connect_active_notify(move |toggle| {
        if updating.get() {
            return;
        }
        if let Some(device) = devices.borrow().get(selector.selected() as usize) {
            let _ = commands.send(AudioCommand::SetMute {
                kind,
                index: device.index,
                muted: toggle.is_active(),
            });
        }
    });
}

fn poll_events(ui: Rc<Controls>, service: AudioService) {
    let refresh = service.commands.clone();
    gtk4::glib::timeout_add_local(Duration::from_secs(2), move || {
        let _ = refresh.send(AudioCommand::Refresh);
        gtk4::glib::ControlFlow::Continue
    });
    gtk4::glib::timeout_add_local(Duration::from_millis(40), move || {
        while let Ok(event) = service.events.try_recv() {
            match event {
                AudioEvent::Snapshot(state) => {
                    ui.apply_snapshot(state);
                    ui.error_revealer.set_reveal_child(false);
                }
                AudioEvent::Level(level) => {
                    let selected = ui.input.selector.selected() as usize;
                    let muted = ui
                        .input
                        .selected
                        .borrow()
                        .get(selected)
                        .is_some_and(|device| device.muted);
                    ui.level.set_fraction(if muted { 0.0 } else { level });
                    ui.meter_state.set_text(if muted {
                        "已静音"
                    } else if level > 0.02 {
                        "正在接收"
                    } else {
                        "无信号"
                    });
                }
                AudioEvent::Error(message) => {
                    ui.error.set_text(&message);
                    ui.error_revealer.set_reveal_child(true);
                }
            }
        }
        gtk4::glib::ControlFlow::Continue
    });
}
