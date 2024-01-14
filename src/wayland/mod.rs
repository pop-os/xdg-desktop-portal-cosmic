use cosmic::cosmic_theme::palette::stimulus::IntoStimulus;
use cosmic_client_toolkit::{
    cosmic_protocols::screencopy::v1::client::{
        zcosmic_screencopy_manager_v1, zcosmic_screencopy_session_v1,
    },
    screencopy::{
        BufferInfo, ScreencopyHandler, ScreencopySessionData, ScreencopySessionDataExt,
        ScreencopyState,
    },
    sctk::{
        self,
        dmabuf::{DmabufFeedback, DmabufFormat, DmabufHandler, DmabufState},
        output::{OutputHandler, OutputInfo, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
        shm::{Shm, ShmHandler},
    },
    toplevel_info::ToplevelInfoState,
    workspace::WorkspaceState,
};
use cosmic_protocols::{
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    workspace::v1::client::zcosmic_workspace_handle_v1,
};
use image::imageops::interpolate_nearest;
use rustix::fd::{self, FromRawFd, RawFd};
use std::{
    collections::HashMap,
    fs,
    hash::Hash,
    io::{self, Write},
    os::{
        fd::{AsFd, OwnedFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    process,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm, wl_shm_pool},
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};

use crate::buffer;

mod toplevel;
mod workspaces;

#[derive(Clone)]
pub struct DmabufHelper {
    feedback: Arc<DmabufFeedback>,
    gbm: Arc<Mutex<gbm::Device<fs::File>>>,
}

impl DmabufHelper {
    // TODO: consider scanout flag?
    // Consider tranches in some way?
    fn feedback_formats(&self) -> impl Iterator<Item = &DmabufFormat> {
        self.feedback
            .tranches()
            .iter()
            .flat_map(|x| x.formats.iter())
            .filter_map(|x| self.feedback.format_table().get(*x as usize))
    }

    pub fn modifiers_for_format(&self, format: u32) -> impl Iterator<Item = u64> + '_ {
        self.feedback_formats()
            .filter(move |x| x.format == format)
            .map(|x| x.modifier)
    }

    pub fn gbm(&self) -> &Mutex<gbm::Device<fs::File>> {
        &self.gbm
    }
}

struct WaylandHelperInner {
    conn: wayland_client::Connection,
    outputs: Mutex<Vec<wl_output::WlOutput>>,
    output_infos: Mutex<HashMap<wl_output::WlOutput, OutputInfo>>,
    output_toplevels: Mutex<HashMap<wl_output::WlOutput, Vec<ZcosmicToplevelHandleV1>>>,
    qh: QueueHandle<AppData>,
    screencopy_manager: zcosmic_screencopy_manager_v1::ZcosmicScreencopyManagerV1,
    wl_shm: wl_shm::WlShm,
    dmabuf: Mutex<Option<DmabufHelper>>,
    zwp_dmabuf: ZwpLinuxDmabufV1,
}

// TODO seperate state object from what is passed to threads
#[derive(Clone)]
pub struct WaylandHelper {
    inner: Arc<WaylandHelperInner>,
}

struct AppData {
    wayland_helper: WaylandHelper, // TODO: populate outputs
    registry_state: RegistryState,
    screencopy_state: ScreencopyState,
    output_state: OutputState,
    shm_state: Shm,
    dmabuf_state: DmabufState,
    toplevel_info_state: ToplevelInfoState,
    workspace_state: WorkspaceState,
}

impl AppData {
    pub fn update_output_toplevels(&self) {
        let toplevels = self.toplevel_info_state.toplevels();
        let mut guard = self
            .wayland_helper
            .inner
            .as_ref()
            .output_toplevels
            .lock()
            .unwrap();
        *guard = toplevels
            .filter_map(|toplevel| {
                let Some(info) = toplevel.1 else {
                    return None;
                };

                let Some(o) = self
                    .workspace_state
                    .workspace_groups()
                    .into_iter()
                    .find_map(|wg| {
                        wg.workspaces.iter().find_map(|w| {
                            info.workspace
                                .iter()
                                .any(|x| {
                                    x != &w.handle
                                        || !w.state.contains(&WEnum::Value(
                                            zcosmic_workspace_handle_v1::State::Active,
                                        ))
                                })
                                .then(|| info.output.iter().cloned().collect::<Vec<_>>())
                        })
                    })
                else {
                    return None;
                };

                Some((o, toplevel.0))
            })
            .fold(
                std::collections::HashMap::new(),
                |mut map, (outputs, toplevel)| {
                    for o in outputs {
                        map.entry(o).or_insert_with(Vec::new).push(toplevel.clone());
                    }
                    map
                },
            );
    }
}

#[derive(Default)]
struct SessionInner {
    buffer_infos: Option<Vec<BufferInfo>>,
    res: Option<Result<(), WEnum<zcosmic_screencopy_session_v1::FailureReason>>>,
}

// TODO: dmabuf? need to handle modifier negotation
#[derive(Default)]
struct Session {
    condvar: Condvar,
    inner: Mutex<SessionInner>,
}

#[derive(Default)]
struct SessionData {
    session: Arc<Session>,
    session_data: ScreencopySessionData,
}

impl Session {
    pub fn for_session(
        session: &zcosmic_screencopy_session_v1::ZcosmicScreencopySessionV1,
    ) -> Option<&Self> {
        Some(&session.data::<SessionData>()?.session)
    }

    fn update<F: FnOnce(&mut SessionInner)>(&self, f: F) {
        f(&mut self.inner.lock().unwrap());
        self.condvar.notify_all();
    }

    fn wait_while<F: FnMut(&SessionInner) -> bool>(&self, mut f: F) -> MutexGuard<SessionInner> {
        self.condvar
            .wait_while(self.inner.lock().unwrap(), |data| f(data))
            .unwrap()
    }
}

pub enum CaptureSource<'a> {
    Output(&'a wl_output::WlOutput),
    Toplevel(&'a ZcosmicToplevelHandleV1),
}

impl WaylandHelper {
    pub fn new(conn: wayland_client::Connection) -> Self {
        // XXX unwrap
        let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
        let qh = event_queue.handle();
        let registry_state = RegistryState::new(&globals);
        let screencopy_state = ScreencopyState::new(&globals, &qh);
        let shm_state = Shm::bind(&globals, &qh).unwrap();
        let zwp_dmabuf = globals.bind(&qh, 4..=4, sctk::globals::GlobalData).unwrap();
        let wayland_helper = WaylandHelper {
            inner: Arc::new(WaylandHelperInner {
                conn,
                outputs: Mutex::new(Vec::new()),
                output_infos: Mutex::new(HashMap::new()),
                output_toplevels: Mutex::new(HashMap::new()),
                qh: qh.clone(),
                screencopy_manager: screencopy_state.screencopy_manager.clone(),
                wl_shm: shm_state.wl_shm().clone(),
                dmabuf: Mutex::new(None),
                zwp_dmabuf,
            }),
        };
        let dmabuf_state = DmabufState::new(&globals, &qh);
        let _ = dmabuf_state.get_default_feedback(&qh);
        let mut data = AppData {
            // XXX must be before workspace and toplevel_info
            output_state: OutputState::new(&globals, &qh),
            shm_state,
            wayland_helper: wayland_helper.clone(),
            screencopy_state,
            dmabuf_state,
            // XXX must be before toplevel_info
            workspace_state: WorkspaceState::new(&registry_state, &qh),
            toplevel_info_state: ToplevelInfoState::new(&registry_state, &qh),
            registry_state,
        };
        event_queue.flush().unwrap();

        event_queue.roundtrip(&mut data).unwrap();

        thread::spawn(move || loop {
            event_queue.blocking_dispatch(&mut data).unwrap();
        });

        wayland_helper
    }

    pub fn dmabuf(&self) -> Option<DmabufHelper> {
        self.inner.dmabuf.lock().unwrap().clone()
    }

    pub fn outputs(&self) -> Vec<wl_output::WlOutput> {
        // TODO Good way to avoid allocation?
        self.inner.outputs.lock().unwrap().clone()
    }

    pub fn output_info(&self, output: &wl_output::WlOutput) -> Option<OutputInfo> {
        self.inner.output_infos.lock().unwrap().get(output).cloned()
    }

    fn set_output_info(&self, output: &wl_output::WlOutput, output_info_opt: Option<OutputInfo>) {
        let mut output_infos = self.inner.output_infos.lock().unwrap();
        match output_info_opt {
            Some(output_info) => {
                output_infos.insert(output.clone(), output_info);
            }
            None => {
                output_infos.remove(output);
            }
        }
    }

    pub fn capture_output_toplevels_shm(
        &self,
        output: &wl_output::WlOutput,
        _overlay_cursor: bool,
    ) -> Vec<ShmImage<OwnedFd>> {
        use std::ffi::CStr;

        // get the active workspace for this output
        // get the toplevels for that workspace
        // capture each toplevel

        let guard = self.inner.output_toplevels.lock().unwrap();
        let Some(toplevels) = guard.get(output) else {
            return Vec::new();
        };

        toplevels
            .into_iter()
            .filter_map(|t| {
                self.capture_source_shm_fd(
                    CaptureSource::Toplevel(t),
                    _overlay_cursor,
                    rustix::fs::memfd_create(
                        unsafe { CStr::from_bytes_with_nul_unchecked(b"pipewire-screencopy\0") },
                        rustix::fs::MemfdFlags::CLOEXEC,
                    )
                    .ok()?,
                    None,
                )
            })
            .collect()
    }

    pub fn capture_source_shm_fd<Fd: AsFd>(
        &self,
        source: CaptureSource,
        overlay_cursor: bool,
        fd: Fd,
        len: Option<u32>,
    ) -> Option<ShmImage<Fd>> {
        // XXX error type?
        // TODO: way to get cursor metadata?

        #[allow(unused_variables)] // TODO
        let overlay_cursor = if overlay_cursor { 1 } else { 0 };

        let session = Arc::new(Session::default());
        let screencopy_session = match source {
            CaptureSource::Output(o) => self.inner.screencopy_manager.capture_output(
                &o,
                zcosmic_screencopy_manager_v1::CursorMode::Hidden, // XXX take into account adventised capabilities
                &self.inner.qh,
                SessionData {
                    session: session.clone(),
                    session_data: Default::default(),
                },
            ),
            CaptureSource::Toplevel(t) => self.inner.screencopy_manager.capture_toplevel(
                &t,
                zcosmic_screencopy_manager_v1::CursorMode::Hidden, // XXX take into account adventised capabilities
                &self.inner.qh,
                SessionData {
                    session: session.clone(),
                    session_data: Default::default(),
                },
            ),
        };
        self.inner.conn.flush().unwrap();

        let buffer_infos = session
            .wait_while(|data| data.buffer_infos.is_none())
            .buffer_infos
            .take()
            .unwrap();

        // XXX
        let buffer_info = buffer_infos
            .iter()
            .find(|x| {
                x.type_ == WEnum::Value(zcosmic_screencopy_session_v1::BufferType::WlShm)
                    && x.format == wl_shm::Format::Abgr8888.into()
            })
            .unwrap();

        let buf_len = buffer_info.stride * buffer_info.height;
        if let Some(len) = len {
            if len != buf_len {
                return None;
            }
        } else {
            if let Err(err) = rustix::fs::ftruncate(&fd, buf_len as _) {};
        };
        let pool = self
            .inner
            .wl_shm
            .create_pool(fd.as_fd(), buf_len as i32, &self.inner.qh, ());
        let buffer = pool.create_buffer(
            0,
            buffer_info.width as i32,
            buffer_info.height as i32,
            buffer_info.stride as i32,
            wl_shm::Format::Abgr8888,
            &self.inner.qh,
            (),
        );

        screencopy_session.attach_buffer(&buffer, None, 0); // XXX age?
        screencopy_session.commit(zcosmic_screencopy_session_v1::Options::empty());
        self.inner.conn.flush().unwrap();

        // TODO: wait for server to release buffer?
        let res = session
            .wait_while(|data| data.res.is_none())
            .res
            .take()
            .unwrap();
        pool.destroy();
        buffer.destroy();

        //std::thread::sleep(std::time::Duration::from_millis(16));

        if res.is_ok() {
            Some(ShmImage {
                fd,
                width: buffer_info.width,
                height: buffer_info.height,
            })
        } else {
            None
        }
    }

    pub fn capture_output_dmabuf_fd<Fd: AsFd>(
        &self,
        output: &wl_output::WlOutput,
        _overlay_cursor: bool,
        dmabuf: &buffer::Dmabuf<Fd>,
    ) {
        // TODO ensure dmabuf is valid format with right number of planes?
        // - params.add can raise protocol error
        let params = self
            .inner
            .zwp_dmabuf
            .create_params(&self.inner.qh, sctk::globals::GlobalData);
        let modifier = u64::from(dmabuf.modifier);
        let modifier_hi = (modifier >> 32) as u32;
        let modifier_lo = (modifier & 0xffffffff) as u32;
        for (i, plane) in dmabuf.planes.iter().enumerate() {
            params.add(
                plane.fd.as_fd(),
                i as u32,
                plane.offset,
                plane.stride,
                modifier_hi,
                modifier_lo,
            );
        }
        // XXX use create
        let buffer = params.create_immed(
            dmabuf.width as i32,
            dmabuf.height as i32,
            dmabuf.format as u32,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.inner.qh,
            (),
        );

        // TODO buffer_infos

        let session = Arc::new(Session::default());
        let screencopy_session = self.inner.screencopy_manager.capture_output(
            output,
            zcosmic_screencopy_manager_v1::CursorMode::Hidden, // XXX take into account adventised capabilities
            &self.inner.qh,
            SessionData {
                session: session.clone(),
                session_data: Default::default(),
            },
        );

        screencopy_session.attach_buffer(&buffer, None, 0); // XXX age?
        screencopy_session.commit(zcosmic_screencopy_session_v1::Options::empty());
        self.inner.conn.flush().unwrap();

        // TODO: wait for server to release buffer?
        let res = session
            .wait_while(|data| data.res.is_none())
            .res
            .take()
            .unwrap();
        buffer.destroy();
    }

    pub fn capture_output_shm(
        &self,
        output: &wl_output::WlOutput,
        overlay_cursor: bool,
    ) -> Option<ShmImage<OwnedFd>> {
        use std::ffi::CStr;
        let name = unsafe { CStr::from_bytes_with_nul_unchecked(b"pipewire-screencopy\0") };
        let fd = rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC).unwrap(); // XXX

        self.capture_source_shm_fd(CaptureSource::Output(output), overlay_cursor, fd, None)
    }
}

pub struct ShmImage<T: AsFd> {
    fd: T,
    pub width: u32,
    pub height: u32,
}

impl<T: AsFd> ShmImage<T> {
    pub fn image(&self) -> anyhow::Result<image::RgbaImage> {
        let mmap = unsafe { memmap2::Mmap::map(&self.fd.as_fd())? };
        image::RgbaImage::from_raw(self.width, self.height, mmap.to_vec())
            .ok_or_else(|| anyhow::anyhow!("ShmImage had incorrect size"))
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers!(OutputState);
}

impl ShmHandler for AppData {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let output_info_opt = self.output_state.info(&output);
        self.wayland_helper
            .set_output_info(&output, output_info_opt);

        self.wayland_helper
            .inner
            .outputs
            .lock()
            .unwrap()
            .push(output);
        self.update_output_toplevels();
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let output_info_opt = self.output_state.info(&output);
        self.wayland_helper
            .set_output_info(&output, output_info_opt);
        self.update_output_toplevels();
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.wayland_helper.set_output_info(&output, None);

        let mut outputs = self.wayland_helper.inner.outputs.lock().unwrap();
        let idx = outputs.iter().position(|x| x == &output).unwrap();
        outputs.remove(idx);
        self.update_output_toplevels();
    }
}

impl ScreencopyHandler for AppData {
    fn screencopy_state(&mut self) -> &mut ScreencopyState {
        &mut self.screencopy_state
    }

    fn init_done(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        session: &zcosmic_screencopy_session_v1::ZcosmicScreencopySessionV1,
        buffer_infos: &[BufferInfo],
    ) {
        Session::for_session(session).unwrap().update(|data| {
            data.buffer_infos = Some(buffer_infos.to_vec());
        });
    }

    fn ready(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        session: &zcosmic_screencopy_session_v1::ZcosmicScreencopySessionV1,
    ) {
        Session::for_session(session).unwrap().update(|data| {
            data.res = Some(Ok(()));
        });
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        session: &zcosmic_screencopy_session_v1::ZcosmicScreencopySessionV1,
        reason: WEnum<zcosmic_screencopy_session_v1::FailureReason>,
    ) {
        // TODO send message to thread
        Session::for_session(session).unwrap().update(|data| {
            data.res = Some(Err(reason));
        });
    }
}

impl DmabufHandler for AppData {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_feedback(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _proxy: &ZwpLinuxDmabufFeedbackV1,
        feedback: DmabufFeedback,
    ) {
        // We only create default feedback, so we assume that's what compositor is sending

        let mut dmabuf = self.wayland_helper.inner.dmabuf.lock().unwrap();
        let gbm = match dmabuf.take() {
            // Change to main device is not likely to happen
            Some(dmabuf) if dmabuf.feedback.main_device() == feedback.main_device() => dmabuf.gbm,
            _ => match gbm_device(feedback.main_device()) {
                Ok(Some(gbm)) => Arc::new(Mutex::new(gbm)),
                Ok(None) => {
                    log::error!(
                        "GBM device not found for main device '{}'",
                        feedback.main_device()
                    );
                    return;
                }
                Err(err) => {
                    log::error!("Failed to open GBM device: {}", err);
                    return;
                }
            },
        };
        *dmabuf = Some(DmabufHelper {
            feedback: Arc::new(feedback),
            gbm,
        });
    }

    fn created(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _params: &ZwpLinuxBufferParamsV1,
        _buffer: wl_buffer::WlBuffer,
    ) {
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _params: &ZwpLinuxBufferParamsV1,
    ) {
    }

    fn released(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _buffer: &wl_buffer::WlBuffer,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppData {
    fn event(
        _app_data: &mut Self,
        _buffer: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppData {
    fn event(
        _app_data: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

// Connect to wayland and start task reading events from socket
pub fn connect_to_wayland() -> wayland_client::Connection {
    let portal_socket = std::env::var("PORTAL_WAYLAND_SOCKET")
        .ok()
        .and_then(|x| x.parse::<RawFd>().ok())
        .map(|fd| unsafe { UnixStream::from_raw_fd(fd) })
        .expect("Failed to connect to PORTAL_WAYLAND_SOCKET");

    wayland_client::Connection::from_socket(portal_socket).unwrap_or_else(|err| {
        log::error!("{}", err);
        process::exit(1)
    })
}

fn gbm_device(rdev: u64) -> io::Result<Option<gbm::Device<fs::File>>> {
    for i in fs::read_dir("/dev/dri")? {
        let i = i?;
        if i.metadata()?.rdev() == rdev {
            let file = fs::File::options()
                .read(true)
                .write(true)
                .open(i.path())
                .unwrap();
            return Ok(Some(gbm::Device::new(file)?));
        }
    }
    Ok(None)
}

impl ScreencopySessionDataExt for SessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.session_data
    }
}

sctk::delegate_shm!(AppData);
sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_dmabuf!(AppData);
cosmic_client_toolkit::delegate_screencopy!(AppData, session: [SessionData]);
