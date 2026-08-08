mod audio;

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use audio::{
    AudioCommand, AudioDevice, AudioEvent, AudioService, EndpointKind, EndpointState,
    volume_confirmed,
};
use gtk4::{
    ApplicationWindow, Button, DropDown, Image, Label, Orientation, ProgressBar, Scale, StringList,
    Switch, prelude::*,
};
use gtk4_layer_shell::{Edge, Layer, LayerShell};

const APP_ID: &str = "top.hxdbk.gtk-layer-sound";
const WINDOW_WIDTH: i32 = 480;
const WINDOW_HEIGHT: i32 = 360;
const BIAS: i32 = 1300;

fn main() {
    let toggle_requested = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&toggle_requested))
        .expect("failed to register window toggle signal");
    let application = gtk4::Application::new(Some(APP_ID), Default::default());
    // 面板常驻：首次激活时创建，之后的每次激活（重复点击）只负责显示/隐藏。
    let panel = Rc::new(RefCell::new(None::<Panel>));
    application.connect_activate(move |app| {
        let mut slot = panel.borrow_mut();
        if slot.is_none() {
            *slot = Some(Panel::create(app, Arc::clone(&toggle_requested)));
        } else {
            slot.as_ref().unwrap().toggle();
        }
    });
    application.run();
}

/// 常驻的声音面板：窗口、UI 与可见状态都只创建一次。
struct Panel {
    window: ApplicationWindow,
    visible: Rc<Cell<bool>>,
}

impl Panel {
    fn create(app: &gtk4::Application, toggle_requested: Arc<AtomicBool>) -> Self {
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
        let visible = Rc::new(Cell::new(true));
        poll_events(Rc::clone(&ui), service);
        poll_window_toggle(window.clone(), toggle_requested, Rc::clone(&visible));
        window.present();
        Self { window, visible }
    }
    fn toggle(&self) {
        toggle_window(&self.window, &self.visible);
    }
}

fn toggle_window(window: &ApplicationWindow, visible: &Cell<bool>) {
    if visible.replace(!visible.get()) {
        window.set_visible(false);
    } else {
        window.present();
    }
}

fn poll_window_toggle(
    window: ApplicationWindow,
    toggle_requested: Arc<AtomicBool>,
    visible: Rc<Cell<bool>>,
) {
    gtk4::glib::timeout_add_local(Duration::from_millis(16), move || {
        if toggle_requested.swap(false, Ordering::Relaxed) {
            toggle_window(&window, &visible);
        }
        gtk4::glib::ControlFlow::Continue
    });
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
    /// 用户拖动音量后、等待 PulseAudio 快照确认的请求值；确认前快照不得回写滑竿与数值。
    /// `None` 表示没有待确认的请求，快照可以自由写回。
    pending: Rc<Cell<Option<f64>>>,
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

/// 构建一个左对齐、带样式类的标签。
fn label(text: &str, class: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class(class);
    label
}

impl Controls {
    fn new() -> Self {
        let root = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();
        root.add_css_class("sound-panel");

        let refresh = Button::builder().label("刷新").build();
        refresh.add_css_class("refresh-button");
        refresh.set_tooltip_text(Some("重新扫描音频设备"));
        let heading = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(1)
            .hexpand(true)
            .build();
        heading.append(&label("声音", "panel-title"));
        heading.append(&label("输出与输入设备", "panel-description"));
        let header = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(9)
            .build();
        header.add_css_class("panel-header");
        header.append(&heading);
        header.append(&refresh);
        root.append(&header);

        let output = endpoint_widgets(
            EndpointKind::Output,
            "输出设备",
            "耳机、扬声器和其他播放设备",
        );
        let input = endpoint_widgets(EndpointKind::Input, "麦克风", "输入电平与录音设备");

        let level = ProgressBar::new();
        level.set_hexpand(true);
        level.add_css_class("input-level");
        let meter_state = label("等待信号", "meter-state");
        let meter = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(10)
            .build();
        meter.add_css_class("meter-row");
        meter.append(&label("输入电平", "meter-label"));
        meter.append(&level);
        meter.append(&meter_state);
        input.card.append(&meter);

        root.append(&output.card);
        root.append(&input.card);

        let error = label("", "error-text");
        error.set_wrap(true);
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
            // 仅当快照已确认用户的音量请求（或无待确认请求）时才回写滑竿与数值；
            // 发送命令前生成的旧快照不得把滑竿拽回旧值（否则会再次触发 value_changed
            // 并把旧值发回 PulseAudio，导致音量被主动还原）。
            if volume_confirmed(device.volume, endpoint.pending.get()) {
                endpoint.pending.set(None);
                endpoint.volume.set_value(device.volume * 100.0);
                endpoint.mute.set_active(device.muted);
                endpoint
                    .value
                    .set_text(&format!("{:.0}%", device.volume * 100.0));
            }
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
        .spacing(7)
        .build();
    card.add_css_class("endpoint-card");
    card.add_css_class(match kind {
        EndpointKind::Output => "output-card",
        EndpointKind::Input => "input-card",
    });
    let head = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(10)
        .build();
    let icon_name = match kind {
        EndpointKind::Output => "audio-speakers-symbolic",
        EndpointKind::Input => "audio-input-microphone-symbolic",
    };
    let icon = Image::from_icon_name(icon_name);
    icon.set_pixel_size(18);
    icon.set_halign(gtk4::Align::Center);
    icon.set_valign(gtk4::Align::Center);
    icon.add_css_class("endpoint-icon");
    let labels = gtk4::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(1)
        .hexpand(true)
        .build();
    labels.append(&label(heading, "endpoint-title"));
    labels.append(&label(subtitle, "endpoint-subtitle"));
    let mute_group = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(7)
        .valign(gtk4::Align::Center)
        .build();
    mute_group.add_css_class("mute-group");
    mute_group.append(&label("静音", "control-label"));
    let mute = Switch::new();
    mute.add_css_class("mute-switch");
    mute.set_tooltip_text(Some("切换设备静音"));
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
    let selector_label = label("设备", "control-label");
    selector_label.set_width_chars(4);
    selector_row.append(&selector_label);
    selector_row.append(&selector);
    let volume_label = label("音量", "control-label");
    volume_label.set_width_chars(4);
    let volume = Scale::with_range(Orientation::Horizontal, 0.0, 150.0, 1.0);
    volume.set_draw_value(false);
    volume.set_hexpand(true);
    volume.add_css_class("volume-scale");
    volume.set_tooltip_text(Some("调节设备音量，最高 150%"));
    let value = label("0%", "volume-value");
    value.set_width_chars(4);
    let volume_row = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(10)
        .build();
    volume_row.append(&volume_label);
    volume_row.append(&volume);
    volume_row.append(&value);
    card.append(&head);
    card.append(&selector_row);
    card.append(&volume_row);
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
        pending: Rc::new(Cell::new(None)),
    }
}

fn bind_controls(ui: &Controls, commands: std::sync::mpsc::Sender<AudioCommand>) {
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
    let pending = Rc::clone(&endpoint.pending);
    let tx = commands.clone();
    // 防抖计时器：按最后一次拖动重新计时，停止拖动 80ms 后才真正发送命令。
    let debounce = Rc::new(RefCell::new(None::<gtk4::glib::SourceId>));
    // 兜底解锁计时器：快照迟迟无法确认请求值（设备封顶、蓝牙量化等）时，超时后放行回写。
    let give_up = Rc::new(RefCell::new(None::<gtk4::glib::SourceId>));
    let selector_debounce = Rc::clone(&debounce);
    let selector_give_up = Rc::clone(&give_up);
    endpoint.selector.connect_selected_notify(move |selector| {
        if updating.get() {
            return;
        }
        // 切换设备后旧请求不再有意义：先取消未触发的防抖与兜底计时器，再作废待确认
        // 音量，防止把设备 A 的拖动值误发到设备 B 上。
        if let Some(source) = selector_debounce.borrow_mut().take() {
            let _ = source.remove();
        }
        if let Some(source) = selector_give_up.borrow_mut().take() {
            let _ = source.remove();
        }
        pending.set(None);
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
    let pending = Rc::clone(&endpoint.pending);
    let selector = endpoint.selector.clone();
    let value = endpoint.value.clone();
    let tx = commands.clone();
    let latest_volume = Rc::new(Cell::new(0.0));
    endpoint.volume.connect_value_changed(move |scale| {
        if updating.get() {
            return;
        }
        let percent = scale.value();
        latest_volume.set(percent / 100.0);
        // 立即反馈数值，不等 PulseAudio 快照回来。
        value.set_text(&format!("{percent:.0}%"));
        // 锁定快照回写：只有快照确认了该值（或兜底超时）后才允许写回滑竿。
        pending.set(Some(percent / 100.0));
        // 用户重新拖动即作废上一次的兜底计时，以最新请求为准。
        if let Some(source) = give_up.borrow_mut().take() {
            let _ = source.remove();
        }
        // 防抖按最后一次变化重新计时，而不是只等待一次 80ms。
        if let Some(source) = debounce.borrow_mut().take() {
            let _ = source.remove();
        }
        let debounce_timer = Rc::clone(&debounce);
        let give_up_timer = Rc::clone(&give_up);
        let pending_target = Rc::clone(&pending);
        let latest = Rc::clone(&latest_volume);
        let devices = Rc::clone(&devices);
        let selector = selector.clone();
        let tx = tx.clone();
        let source = gtk4::glib::timeout_add_local_once(Duration::from_millis(80), move || {
            debounce_timer.borrow_mut().take();
            if let Some(device) = devices.borrow().get(selector.selected() as usize) {
                let _ = tx.send(AudioCommand::SetVolume {
                    kind,
                    index: device.index,
                    volume: latest.get(),
                });
            }
            // 发送后仍保持锁定，直到快照确认回显值；确认不了时 3 秒后兜底解锁，
            // 让下一次快照把滑竿同步回真实值。
            if let Some(source) = give_up_timer.borrow_mut().take() {
                let _ = source.remove();
            }
            let give_up_source = Rc::clone(&give_up_timer);
            let pending_target = Rc::clone(&pending_target);
            let source = gtk4::glib::timeout_add_local_once(Duration::from_millis(3000), move || {
                give_up_source.borrow_mut().take();
                pending_target.set(None);
            });
            give_up_timer.replace(Some(source));
        });
        debounce.replace(Some(source));
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
