//! The audio boundary deliberately contains no GTK types.  It makes the UI easy to exercise with
//! a fake backend and keeps PulseAudio's threaded API on one worker thread.
use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{Context, FlagSet as ContextFlags},
    mainloop::threaded::Mainloop,
    stream::{PeekResult, Stream},
    volume::{ChannelVolumes, Volume},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    Output,
    Input,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioDevice {
    pub index: u32,
    pub channels: u8,
    pub name: String,
    pub label: String,
    pub volume: f64,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EndpointState {
    pub kind: EndpointKind,
    pub devices: Vec<AudioDevice>,
    pub default_name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AudioCommand {
    Refresh,
    Select {
        kind: EndpointKind,
        index: u32,
    },
    SetVolume {
        kind: EndpointKind,
        index: u32,
        volume: f64,
    },
    SetMute {
        kind: EndpointKind,
        index: u32,
        muted: bool,
    },
    #[allow(dead_code)]
    Stop,
}

#[derive(Debug, Clone)]
pub enum AudioEvent {
    Snapshot(EndpointState),
    Level(f64),
    Error(String),
}

pub trait AudioBackend: 'static {
    fn refresh(&mut self) -> Result<Vec<EndpointState>, String>;
    fn select(&mut self, kind: EndpointKind, index: u32) -> Result<(), String>;
    fn set_volume(&mut self, kind: EndpointKind, index: u32, volume: f64) -> Result<(), String>;
    fn set_mute(&mut self, kind: EndpointKind, index: u32, muted: bool) -> Result<(), String>;
    fn microphone_level(&mut self) -> Result<f64, String>;
}

pub struct AudioService {
    pub commands: Sender<AudioCommand>,
    pub events: Receiver<AudioEvent>,
}

impl AudioService {
    pub fn spawn() -> Self {
        Self::with_backend(PulseBackend::new)
    }

    pub fn with_backend<B, F>(factory: F) -> Self
    where
        B: AudioBackend,
        F: FnOnce() -> Result<B, String> + Send + 'static,
    {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || worker(factory, command_rx, event_tx));
        Self {
            commands: command_tx,
            events: event_rx,
        }
    }
}

fn worker<B: AudioBackend>(
    factory: impl FnOnce() -> Result<B, String>,
    commands: Receiver<AudioCommand>,
    events: Sender<AudioEvent>,
) {
    let Ok(mut backend) = factory() else {
        let _ = events.send(AudioEvent::Error(
            "无法连接 PulseAudio / PipeWire-Pulse".into(),
        ));
        return;
    };
    publish_snapshot(&mut backend, &events);
    loop {
        match commands.recv_timeout(Duration::from_millis(75)) {
            Ok(AudioCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(AudioCommand::Refresh) => publish_snapshot(&mut backend, &events),
            Ok(AudioCommand::Select { kind, index }) => {
                run_command(&events, backend.select(kind, index), &mut backend)
            }
            Ok(AudioCommand::SetVolume {
                kind,
                index,
                volume,
            }) => run_command_no_refresh(
                &events,
                backend.set_volume(kind, index, volume.clamp(0.0, 1.5)),
            ),
            Ok(AudioCommand::SetMute { kind, index, muted }) => {
                run_command_no_refresh(&events, backend.set_mute(kind, index, muted))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => match backend.microphone_level() {
                Ok(level) => {
                    let _ = events.send(AudioEvent::Level(level.clamp(0.0, 1.0)));
                }
                Err(error) => {
                    let _ = events.send(AudioEvent::Error(format!("麦克风电平不可用：{error}")));
                }
            },
        }
    }
}
fn run_command_no_refresh(events: &Sender<AudioEvent>, result: Result<(), String>) {
    if let Err(error) = result {
        let _ = events.send(AudioEvent::Error(error));
    }
}

fn run_command<B: AudioBackend>(
    events: &Sender<AudioEvent>,
    result: Result<(), String>,
    backend: &mut B,
) {
    match result {
        Ok(()) => publish_snapshot(backend, events),
        Err(error) => {
            publish_snapshot(backend, events);
            let _ = events.send(AudioEvent::Error(error));
        }
    }
}
fn publish_snapshot<B: AudioBackend>(backend: &mut B, events: &Sender<AudioEvent>) {
    match backend.refresh() {
        Ok(states) => {
            for state in states {
                let _ = events.send(AudioEvent::Snapshot(state));
            }
        }
        Err(error) => {
            let _ = events.send(AudioEvent::Error(error));
        }
    }
}

/// A PulseAudio client. Commands use libpulse directly; the synchronous worker boundary prevents
/// its callback-driven API from ever running on the GTK thread.
struct PulseBackend {
    mainloop: Mainloop,
    context: Context,
    output: EndpointState,
    input: EndpointState,
    meter: Option<Stream>,
}
impl PulseBackend {
    fn new() -> Result<Self, String> {
        let mut mainloop = Mainloop::new().ok_or("无法创建 PulseAudio threaded mainloop")?;
        let mut context =
            Context::new(&mainloop, "iconbar-sound").ok_or("无法创建 PulseAudio context")?;
        context
            .connect(None, ContextFlags::NOFLAGS, None)
            .map_err(|e| format!("{e:?}"))?;
        // Follow the threaded-mainloop contract: connect before starting, then hold the lock while
        // querying state; wait() atomically releases and reacquires that lock for callbacks.
        mainloop.lock();
        if let Err(error) = mainloop.start() {
            mainloop.unlock();
            return Err(format!("{error:?}"));
        }
        mainloop.unlock();
        for _ in 0..50 {
            mainloop.lock();
            let state = context.get_state();
            mainloop.unlock();
            match state {
                pulse::context::State::Ready => break,
                pulse::context::State::Failed | pulse::context::State::Terminated => {
                    let _ = mainloop.stop();
                    return Err("PulseAudio context 已断开".into());
                }
                _ => thread::sleep(Duration::from_millis(20)),
            }
        }
        mainloop.lock();
        let ready = context.get_state() == pulse::context::State::Ready;
        mainloop.unlock();
        if !ready {
            let _ = mainloop.stop();
            return Err("等待 PulseAudio context 超时".into());
        }
        Ok(Self {
            mainloop,
            context,
            output: EndpointState {
                kind: EndpointKind::Output,
                devices: Vec::new(),
                default_name: None,
            },
            input: EndpointState {
                kind: EndpointKind::Input,
                devices: Vec::new(),
                default_name: None,
            },
            meter: None,
        })
    }
    fn endpoint_mut(&mut self, kind: EndpointKind) -> &mut EndpointState {
        if kind == EndpointKind::Output {
            &mut self.output
        } else {
            &mut self.input
        }
    }
    fn selected(&self, kind: EndpointKind, index: u32) -> Result<&AudioDevice, String> {
        self.endpoint(kind)
            .devices
            .iter()
            .find(|device| device.index == index)
            .ok_or_else(|| "选择的音频设备已不可用，请刷新".into())
    }
    fn endpoint(&self, kind: EndpointKind) -> &EndpointState {
        if kind == EndpointKind::Output {
            &self.output
        } else {
            &self.input
        }
    }
    fn volume(percent: f64, channels: u8) -> ChannelVolumes {
        let mut volumes = ChannelVolumes::default();
        volumes.set(
            channels.max(1),
            Volume((Volume::NORMAL.0 as f64 * percent.clamp(0.0, 1.5)) as u32),
        );
        volumes
    }
    fn query_outputs(&mut self) -> Result<Vec<AudioDevice>, String> {
        let (done_tx, done_rx) = mpsc::channel();
        let devices = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&devices);
        self.mainloop.lock();
        let operation = self
            .context
            .introspect()
            .get_sink_info_list(move |result| match result {
                ListResult::Item(info) => collected.lock().unwrap().push(AudioDevice {
                    index: info.index,
                    channels: info.volume.len(),
                    name: info.name.as_deref().unwrap_or_default().to_string(),
                    label: info
                        .description
                        .as_deref()
                        .unwrap_or("未命名输出")
                        .to_string(),
                    volume: info.volume.avg().0 as f64 / Volume::NORMAL.0 as f64,
                    muted: info.mute,
                }),
                ListResult::End | ListResult::Error => {
                    let _ = done_tx.send(());
                }
            });
        self.mainloop.unlock();
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "读取 PulseAudio 输出设备超时")?;
        drop(operation);
        Ok(Arc::try_unwrap(devices).unwrap().into_inner().unwrap())
    }
    fn query_inputs(&mut self) -> Result<Vec<AudioDevice>, String> {
        let (done_tx, done_rx) = mpsc::channel();
        let devices = Arc::new(Mutex::new(Vec::new()));
        let collected = Arc::clone(&devices);
        self.mainloop.lock();
        let operation =
            self.context
                .introspect()
                .get_source_info_list(move |result| match result {
                    ListResult::Item(info) if info.monitor_of_sink.is_none() => {
                        collected.lock().unwrap().push(AudioDevice {
                            index: info.index,
                            channels: info.volume.len(),
                            name: info.name.as_deref().unwrap_or_default().to_string(),
                            label: info
                                .description
                                .as_deref()
                                .unwrap_or("未命名麦克风")
                                .to_string(),
                            volume: info.volume.avg().0 as f64 / Volume::NORMAL.0 as f64,
                            muted: info.mute,
                        })
                    }
                    ListResult::End | ListResult::Error => {
                        let _ = done_tx.send(());
                    }
                    _ => {}
                });
        self.mainloop.unlock();
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "读取 PulseAudio 输入设备超时")?;
        drop(operation);
        Ok(Arc::try_unwrap(devices).unwrap().into_inner().unwrap())
    }
    fn query_defaults(&mut self) -> Result<(Option<String>, Option<String>), String> {
        let (tx, rx) = mpsc::channel();
        self.mainloop.lock();
        let operation = self.context.introspect().get_server_info(move |info| {
            let _ = tx.send((
                info.default_sink_name.as_deref().map(str::to_string),
                info.default_source_name.as_deref().map(str::to_string),
            ));
        });
        self.mainloop.unlock();
        let result = rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "读取 PulseAudio 默认设备超时")?;
        drop(operation);
        Ok(result)
    }
    fn wait_success(rx: Receiver<bool>, action: &str) -> Result<(), String> {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("PulseAudio 拒绝{action}")),
            Err(_) => Err(format!("等待{action}完成超时")),
        }
    }
    fn move_streams(&mut self, kind: EndpointKind, target: u32) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.mainloop.lock();
        if kind == EndpointKind::Output {
            let operation = self
                .context
                .introspect()
                .get_sink_input_info_list(move |result| match result {
                    ListResult::Item(info) => {
                        let _ = tx.send(Some(info.index));
                    }
                    ListResult::End => {
                        let _ = tx.send(None);
                    }
                    ListResult::Error => {
                        let _ = tx.send(Some(u32::MAX));
                    }
                });
            self.mainloop.unlock();
            loop {
                let stream = match rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(Some(u32::MAX)) => return Err("枚举播放流失败".into()),
                    Ok(Some(stream)) => stream,
                    Ok(None) => break,
                    Err(_) => return Err("枚举播放流超时".into()),
                };
                let (done_tx, done_rx) = mpsc::channel();
                self.mainloop.lock();
                self.context.introspect().move_sink_input_by_index(
                    stream,
                    target,
                    Some(Box::new(move |ok| {
                        let _ = done_tx.send(ok);
                    })),
                );
                self.mainloop.unlock();
                Self::wait_success(done_rx, "迁移播放流")?;
            }
            drop(operation);
            return Ok(());
        }
        let operation = self
            .context
            .introspect()
            .get_source_output_info_list(move |result| match result {
                ListResult::Item(info) => {
                    let _ = tx.send(Some(info.index));
                }
                ListResult::End => {
                    let _ = tx.send(None);
                }
                ListResult::Error => {
                    let _ = tx.send(Some(u32::MAX));
                }
            });
        self.mainloop.unlock();
        loop {
            let stream = match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Some(u32::MAX)) => return Err("枚举录音流失败".into()),
                Ok(Some(stream)) => stream,
                Ok(None) => break,
                Err(_) => return Err("枚举录音流超时".into()),
            };
            let (done_tx, done_rx) = mpsc::channel();
            self.mainloop.lock();
            self.context.introspect().move_source_output_by_index(
                stream,
                target,
                Some(Box::new(move |ok| {
                    let _ = done_tx.send(ok);
                })),
            );
            self.mainloop.unlock();
            Self::wait_success(done_rx, "迁移录音流")?;
        }
        drop(operation);
        Ok(())
    }
    fn start_meter(&mut self, source: &str) -> Result<(), String> {
        self.mainloop.lock();
        if let Some(mut meter) = self.meter.take() {
            let _ = meter.disconnect();
        }
        let spec = pulse::sample::Spec {
            format: pulse::sample::Format::S16NE,
            channels: 1,
            rate: 44_100,
        };
        let Some(mut stream) =
            Stream::new(&mut self.context, "iconbar microphone meter", &spec, None)
        else {
            self.mainloop.unlock();
            return Err("无法创建麦克风调试流".into());
        };
        let result =
            stream.connect_record(Some(source), None, pulse::stream::FlagSet::ADJUST_LATENCY);
        self.mainloop.unlock();
        result.map_err(|error| format!("{error:?}"))?;
        self.meter = Some(stream);
        Ok(())
    }
}

impl AudioBackend for PulseBackend {
    fn refresh(&mut self) -> Result<Vec<EndpointState>, String> {
        self.output.devices = self.query_outputs()?;
        self.input.devices = self.query_inputs()?;
        let (sink, source) = self.query_defaults()?;
        self.output.default_name = sink;
        self.input.default_name = source;
        if let Some(source) = self.input.default_name.clone() {
            self.start_meter(&source)?;
        }
        Ok(vec![self.output.clone(), self.input.clone()])
    }
    fn select(&mut self, kind: EndpointKind, index: u32) -> Result<(), String> {
        let device = self.selected(kind, index)?.clone();
        let (done_tx, done_rx) = mpsc::channel();
        self.mainloop.lock();
        let operation = match kind {
            EndpointKind::Output => self.context.set_default_sink(&device.name, move |ok| {
                let _ = done_tx.send(ok);
            }),
            EndpointKind::Input => self.context.set_default_source(&device.name, move |ok| {
                let _ = done_tx.send(ok);
            }),
        };
        self.mainloop.unlock();
        Self::wait_success(done_rx, "切换默认设备")?;
        drop(operation);
        self.move_streams(kind, index)?;
        if kind == EndpointKind::Input {
            self.start_meter(&device.name)?;
        }
        self.endpoint_mut(kind).default_name = Some(device.name);
        Ok(())
    }
    fn set_volume(&mut self, kind: EndpointKind, index: u32, volume: f64) -> Result<(), String> {
        let volumes = Self::volume(volume, self.selected(kind, index)?.channels);
        let (done_tx, done_rx) = mpsc::channel();
        self.mainloop.lock();
        let operation = match kind {
            EndpointKind::Output => self.context.introspect().set_sink_volume_by_index(
                index,
                &volumes,
                Some(Box::new(move |ok| {
                    let _ = done_tx.send(ok);
                })),
            ),
            EndpointKind::Input => self.context.introspect().set_source_volume_by_index(
                index,
                &volumes,
                Some(Box::new(move |ok| {
                    let _ = done_tx.send(ok);
                })),
            ),
        };
        self.mainloop.unlock();
        Self::wait_success(done_rx, "设置音量")?;
        drop(operation);
        if let Some(device) = self
            .endpoint_mut(kind)
            .devices
            .iter_mut()
            .find(|device| device.index == index)
        {
            device.volume = volume;
        }
        Ok(())
    }
    fn set_mute(&mut self, kind: EndpointKind, index: u32, muted: bool) -> Result<(), String> {
        let (done_tx, done_rx) = mpsc::channel();
        self.mainloop.lock();
        let operation = match kind {
            EndpointKind::Output => self.context.introspect().set_sink_mute_by_index(
                index,
                muted,
                Some(Box::new(move |ok| {
                    let _ = done_tx.send(ok);
                })),
            ),
            EndpointKind::Input => self.context.introspect().set_source_mute_by_index(
                index,
                muted,
                Some(Box::new(move |ok| {
                    let _ = done_tx.send(ok);
                })),
            ),
        };
        self.mainloop.unlock();
        Self::wait_success(done_rx, "设置静音")?;
        drop(operation);
        if let Some(device) = self
            .endpoint_mut(kind)
            .devices
            .iter_mut()
            .find(|device| device.index == index)
        {
            device.muted = muted;
        }
        Ok(())
    }
    fn microphone_level(&mut self) -> Result<f64, String> {
        let Some(stream) = self.meter.as_mut() else {
            return Ok(0.0);
        };
        self.mainloop.lock();
        let result = match stream.peek() {
            Ok(PeekResult::Data(bytes)) => {
                let samples = bytes
                    .chunks_exact(2)
                    .map(|bytes| i16::from_ne_bytes([bytes[0], bytes[1]]))
                    .collect::<Vec<_>>();
                let _ = stream.discard();
                Ok(rms_level(&samples))
            }
            Ok(PeekResult::Hole(_)) => {
                let _ = stream.discard();
                Ok(0.0)
            }
            Ok(PeekResult::Empty) => Ok(0.0),
            Err(error) => Err(format!("{error:?}")),
        };
        self.mainloop.unlock();
        result
    }
}

impl Drop for PulseBackend {
    fn drop(&mut self) {
        self.mainloop.lock();
        if let Some(mut meter) = self.meter.take() {
            let _ = meter.disconnect();
        }
        self.context.disconnect();
        self.mainloop.unlock();
        let _ = self.mainloop.stop();
    }
}

/// RMS for interleaved signed 16-bit PCM captured by a PulseAudio record stream.
pub fn rms_level(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples
        .iter()
        .map(|sample| {
            let v = f64::from(*sample) / f64::from(i16::MAX);
            v * v
        })
        .sum::<f64>();
    (sum / samples.len() as f64).sqrt().clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeBackend {
        calls: Arc<Mutex<Vec<String>>>,
        fail_select: bool,
    }
    impl AudioBackend for FakeBackend {
        fn refresh(&mut self) -> Result<Vec<EndpointState>, String> {
            Ok(vec![EndpointState {
                kind: EndpointKind::Output,
                devices: vec![AudioDevice {
                    index: 7,
                    channels: 2,
                    name: "fake.sink".into(),
                    label: "Fake speaker".into(),
                    volume: 0.5,
                    muted: false,
                }],
                default_name: Some("fake.sink".into()),
            }])
        }
        fn select(&mut self, kind: EndpointKind, index: u32) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("select:{kind:?}:{index}"));
            if self.fail_select {
                Err("selection failed".into())
            } else {
                Ok(())
            }
        }
        fn set_volume(
            &mut self,
            kind: EndpointKind,
            index: u32,
            volume: f64,
        ) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("volume:{kind:?}:{index}:{volume:.1}"));
            Ok(())
        }
        fn set_mute(&mut self, kind: EndpointKind, index: u32, muted: bool) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("mute:{kind:?}:{index}:{muted}"));
            Ok(())
        }
        fn microphone_level(&mut self) -> Result<f64, String> {
            Ok(0.25)
        }
    }
    #[test]
    fn rms_is_normalized() {
        assert_eq!(rms_level(&[]), 0.0);
        assert!((rms_level(&[i16::MAX, i16::MAX]) - 1.0).abs() < 0.001);
        assert!((rms_level(&[0, i16::MAX]) - 0.707).abs() < 0.01);
    }
    #[test]
    fn endpoint_state_preserves_kind() {
        let state = EndpointState {
            kind: EndpointKind::Input,
            devices: vec![],
            default_name: None,
        };
        assert_eq!(state.kind, EndpointKind::Input);
    }
    #[test]
    fn worker_publishes_fake_backend_snapshot() {
        let service = AudioService::with_backend(|| {
            Ok(FakeBackend {
                calls: Arc::new(Mutex::new(vec![])),
                fail_select: false,
            })
        });
        match service.events.recv_timeout(Duration::from_secs(1)).unwrap() {
            AudioEvent::Snapshot(state) => assert_eq!(state.devices[0].index, 7),
            other => panic!("expected snapshot, got {other:?}"),
        }
        let _ = service.commands.send(AudioCommand::Stop);
    }
    #[test]
    fn worker_routes_output_input_commands_and_clamps_volume() {
        let calls = Arc::new(Mutex::new(vec![]));
        let factory_calls = Arc::clone(&calls);
        let service = AudioService::with_backend(move || {
            Ok(FakeBackend {
                calls: factory_calls,
                fail_select: false,
            })
        });
        let _ = service.events.recv_timeout(Duration::from_secs(1));
        service
            .commands
            .send(AudioCommand::Select {
                kind: EndpointKind::Output,
                index: 7,
            })
            .unwrap();
        service
            .commands
            .send(AudioCommand::Select {
                kind: EndpointKind::Input,
                index: 8,
            })
            .unwrap();
        service
            .commands
            .send(AudioCommand::SetVolume {
                kind: EndpointKind::Input,
                index: 8,
                volume: 9.0,
            })
            .unwrap();
        service
            .commands
            .send(AudioCommand::SetMute {
                kind: EndpointKind::Output,
                index: 7,
                muted: true,
            })
            .unwrap();
        for _ in 0..4 {
            let _ = service.events.recv_timeout(Duration::from_secs(1));
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "select:Output:7",
                "select:Input:8",
                "volume:Input:8:1.5",
                "mute:Output:7:true"
            ]
        );
        let _ = service.commands.send(AudioCommand::Stop);
    }
    #[test]
    fn backend_error_keeps_last_snapshot() {
        let calls = Arc::new(Mutex::new(vec![]));
        let factory_calls = Arc::clone(&calls);
        let service = AudioService::with_backend(move || {
            Ok(FakeBackend {
                calls: factory_calls,
                fail_select: true,
            })
        });
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(1)).unwrap(),
            AudioEvent::Snapshot(_)
        ));
        service
            .commands
            .send(AudioCommand::Select {
                kind: EndpointKind::Output,
                index: 7,
            })
            .unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(1)).unwrap(),
            AudioEvent::Snapshot(_)
        ));
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(1)).unwrap(),
            AudioEvent::Error(_)
        ));
        let _ = service.commands.send(AudioCommand::Stop);
    }
}
