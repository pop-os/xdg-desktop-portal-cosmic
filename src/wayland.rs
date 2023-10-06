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
        output::{OutputHandler, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
        shm::{Shm, ShmHandler},
    },
};
use std::{
    fs,
    io::{self, Write},
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::MetadataExt,
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
            shm_state,
            wayland_helper: wayland_helper.clone(),
            output_state: OutputState::new(&globals, &qh),
            screencopy_state,
            registry_state,
            dmabuf_state,
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

        self.capture_output_shm_fd(output, overlay_cursor, fd, None)
    }

    pub fn capture_output_shm_fd<T: AsFd>(
        &self,
        output: &wl_output::WlOutput,
        overlay_cursor: bool,
        fd: T,
        len: Option<u32>,
    ) -> Option<ShmImage<T>> {
        // XXX error type?
        // TODO: way to get cursor metadata?

        #[allow(unused_variables)] // TODO
        let overlay_cursor = if overlay_cursor { 1 } else { 0 };

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
            rustix::fs::ftruncate(&fd, buf_len as _);
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
}

pub struct ShmImage<T: AsFd> {
    fd: T,
    pub width: u32,
    pub height: u32,
}

impl<T: AsFd> ShmImage<T> {
    pub fn write_to_png<W: Write>(&mut self, file: W) -> anyhow::Result<()> {
        let mmap = unsafe { memmap2::Mmap::map(&self.fd.as_fd())? };
        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&mmap)?;

        Ok(())
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
        self.wayland_helper
            .inner
            .outputs
            .lock()
            .unwrap()
            .push(output);
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let mut outputs = self.wayland_helper.inner.outputs.lock().unwrap();
        let idx = outputs.iter().position(|x| x == &output).unwrap();
        outputs.remove(idx);
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
    wayland_client::Connection::connect_to_env().unwrap_or_else(|err| {
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
