//! Implements the legacy Notify* input-injection path for RemoteDesktop.
//! cosmic-comp has no Notify* DBus API, so we connect as an ei sender instead.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use ashpd::enumflags2::BitFlags;
use futures::StreamExt;
use reis::ei;
use reis::event::{Device, DeviceCapability, EiEvent};
use rustix::time::{ClockId, clock_gettime};
use tokio::sync::mpsc;

use crate::remote_desktop::{DEVICE_KEYBOARD, DEVICE_POINTER, DEVICE_TOUCHSCREEN};

#[derive(Debug)]
pub enum Command {
    PointerMotion { dx: f64, dy: f64 },
    PointerMotionAbsolute { x: f64, y: f64 },
    PointerButton { button: i32, state: u32 },
    PointerAxis { dx: f64, dy: f64 },
    PointerAxisDiscrete { axis: u32, steps: i32 },
    KeyboardKeycode { keycode: i32, state: u32 },
    KeyboardKeysym { keysym: i32, state: u32 },
    TouchDown { slot: u32, x: f64, y: f64 },
    TouchMotion { slot: u32, x: f64, y: f64 },
    TouchUp { slot: u32 },
}

#[derive(Clone)]
pub struct EiSender {
    tx: mpsc::Sender<Command>,
}

impl EiSender {
    // reis's event stream is !Send so we run the loop on its own thread
    pub async fn connect(stream: UnixStream, device_types: u32) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel(64);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("xdpc-ei-sender".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let connect = async {
                        let context = ei::Context::new(stream)?;
                        context
                            .handshake_tokio(
                                "xdg-desktop-portal-cosmic",
                                ei::handshake::ContextType::Sender,
                            )
                            .await
                            .map_err(|err| {
                                std::io::Error::other(format!("ei handshake failed: {err}"))
                            })
                    };
                    match connect.await {
                        Ok((connection, events)) => {
                            let _ = ready_tx.send(Ok(()));
                            run(connection, events, capabilities_for(device_types), rx).await;
                        }
                        Err(err) => {
                            let _ = ready_tx.send(Err(err));
                        }
                    }
                });
            })?;
        ready_rx
            .await
            .map_err(|_| std::io::Error::other("ei sender thread exited"))??;
        Ok(Self { tx })
    }

    pub fn send(&self, command: Command) {
        if let Err(err) = self.tx.try_send(command) {
            log::warn!("Failed to queue remote desktop input event: {err}");
        }
    }
}

fn capabilities_for(device_types: u32) -> BitFlags<DeviceCapability> {
    let mut caps = BitFlags::empty();
    if device_types & DEVICE_KEYBOARD != 0 {
        caps.insert(DeviceCapability::Keyboard);
        // `Text` carries keysym injection, used by NotifyKeyboardKeysym.
        caps.insert(DeviceCapability::Text);
    }
    if device_types & DEVICE_POINTER != 0 {
        caps.insert(DeviceCapability::Pointer);
        caps.insert(DeviceCapability::PointerAbsolute);
        caps.insert(DeviceCapability::Button);
        caps.insert(DeviceCapability::Scroll);
    }
    if device_types & DEVICE_TOUCHSCREEN != 0 {
        caps.insert(DeviceCapability::Touch);
    }
    caps
}

struct DeviceState {
    emulating: bool,
    sequence: u32,
}

struct State {
    connection: reis::event::Connection,
    caps: BitFlags<DeviceCapability>,
    devices: HashMap<Device, DeviceState>,
}

async fn run(
    connection: reis::event::Connection,
    mut events: reis::tokio::EiConvertEventStream,
    caps: BitFlags<DeviceCapability>,
    mut rx: mpsc::Receiver<Command>,
) {
    let mut state = State {
        connection,
        caps,
        devices: HashMap::new(),
    };
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(Ok(event)) => state.handle_event(event),
                Some(Err(err)) => {
                    log::warn!("Remote desktop ei event stream error: {err}");
                    break;
                }
                None => break,
            },
            command = rx.recv() => match command {
                Some(command) => state.handle_command(command),
                None => break,
            },
        }
    }
    log::debug!("Remote desktop ei sender task ended");
}

impl State {
    fn handle_event(&mut self, event: EiEvent) {
        match event {
            EiEvent::SeatAdded(evt) => {
                evt.seat.bind_capabilities(self.caps);
                let _ = self.connection.flush();
            }
            EiEvent::DeviceAdded(evt) => {
                self.devices.insert(
                    evt.device,
                    DeviceState {
                        emulating: false,
                        sequence: 0,
                    },
                );
            }
            EiEvent::DeviceResumed(evt) => {
                let serial = self.connection.serial();
                if let Some(state) = self.devices.get_mut(&evt.device) {
                    evt.device.device().start_emulating(serial, state.sequence);
                    state.sequence = state.sequence.wrapping_add(1);
                    state.emulating = true;
                    let _ = self.connection.flush();
                }
            }
            EiEvent::DevicePaused(evt) => {
                if let Some(state) = self.devices.get_mut(&evt.device) {
                    state.emulating = false;
                }
            }
            EiEvent::DeviceRemoved(evt) => {
                self.devices.remove(&evt.device);
            }
            _ => {}
        }
    }

    fn emit<T: ei::Interface, F: FnOnce(&T)>(&self, serial: u32, time: u64, f: F) {
        for (device, state) in &self.devices {
            if !state.emulating {
                continue;
            }
            if let Some(interface) = device.interface::<T>() {
                f(&interface);
                device.device().frame(serial, time);
                return;
            }
        }
        log::debug!("No emulating remote desktop device for {}", T::NAME);
    }

    fn handle_command(&self, command: Command) {
        let serial = self.connection.serial();
        let time = monotonic_micros();
        match command {
            Command::PointerMotion { dx, dy } => {
                self.emit::<ei::Pointer, _>(serial, time, |p| {
                    p.motion_relative(dx as f32, dy as f32);
                });
            }
            // `stream` (which output) is ignored for now; treat as a single space.
            Command::PointerMotionAbsolute { x, y } => {
                self.emit::<ei::PointerAbsolute, _>(serial, time, |p| {
                    p.motion_absolute(x as f32, y as f32);
                });
            }
            Command::PointerButton { button, state } => {
                self.emit::<ei::Button, _>(serial, time, |b| {
                    b.button(button as u32, button_state(state));
                });
            }
            Command::PointerAxis { dx, dy } => {
                self.emit::<ei::Scroll, _>(serial, time, |s| {
                    s.scroll(dx as f32, dy as f32);
                });
            }
            Command::PointerAxisDiscrete { axis, steps } => {
                // axis 0 = vertical, 1 = horizontal; ei uses 120 units per detent.
                let (x, y) = if axis == 0 {
                    (0, steps.saturating_mul(120))
                } else {
                    (steps.saturating_mul(120), 0)
                };
                self.emit::<ei::Scroll, _>(serial, time, |s| {
                    s.scroll_discrete(x, y);
                });
            }
            Command::KeyboardKeycode { keycode, state } => {
                self.emit::<ei::Keyboard, _>(serial, time, |k| {
                    k.key(keycode as u32, key_state(state));
                });
            }
            Command::KeyboardKeysym { keysym, state } => {
                self.emit::<ei::Text, _>(serial, time, |t| {
                    t.keysym(keysym as u32, key_state(state));
                });
            }
            Command::TouchDown { slot, x, y } => {
                self.emit::<ei::Touchscreen, _>(serial, time, |t| {
                    t.down(slot, x as f32, y as f32);
                });
            }
            Command::TouchMotion { slot, x, y } => {
                self.emit::<ei::Touchscreen, _>(serial, time, |t| {
                    t.motion(slot, x as f32, y as f32);
                });
            }
            Command::TouchUp { slot } => {
                self.emit::<ei::Touchscreen, _>(serial, time, |t| {
                    t.up(slot);
                });
            }
        }
        let _ = self.connection.flush();
    }
}

fn button_state(state: u32) -> ei::button::ButtonState {
    if state == 0 {
        ei::button::ButtonState::Released
    } else {
        ei::button::ButtonState::Press
    }
}

fn key_state(state: u32) -> ei::keyboard::KeyState {
    if state == 0 {
        ei::keyboard::KeyState::Released
    } else {
        ei::keyboard::KeyState::Press
    }
}

fn monotonic_micros() -> u64 {
    let t = clock_gettime(ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1_000
}
