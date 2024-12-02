use cosmic::iced_winit::platform_specific::wayland::subsurface_widget::Shmbuf;
use cosmic_client_toolkit::{
    cosmic_protocols::screencopy::v2::client::{
        zcosmic_screencopy_frame_v2, zcosmic_screencopy_manager_v2, zcosmic_screencopy_session_v2,
    },
    screencopy::{
        capture, Formats, Frame, ScreencopyFrameData, ScreencopyFrameDataExt, ScreencopyHandler,
        ScreencopySessionData, ScreencopySessionDataExt, ScreencopyState,
    },
    sctk::{
        self,
        dmabuf::{DmabufFeedback, DmabufFormat, DmabufHandler, DmabufState},
        output::{OutputHandler, OutputInfo, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
        shm::{Shm, ShmHandler},
    },
    toplevel_info::{ToplevelInfo, ToplevelInfoState},
    workspace::WorkspaceState,
};
use cosmic_protocols::{
    image_source::v1::client::{
        zcosmic_output_image_source_manager_v1::ZcosmicOutputImageSourceManagerV1,
        zcosmic_toplevel_image_source_manager_v1::ZcosmicToplevelImageSourceManagerV1,
    },
    toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    workspace::v1::client::zcosmic_workspace_handle_v1,
};
use futures::channel::oneshot;
use rustix::fd::{FromRawFd, RawFd};
use std::{
    collections::HashMap,
    env, fs, io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    process,
    sync::{Arc, Condvar, Mutex, Weak},
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
    toplevels: Mutex<Vec<(ZcosmicToplevelHandleV1, ToplevelInfo)>>,
    qh: QueueHandle<AppData>,
    screencopy_manager: zcosmic_screencopy_manager_v2::ZcosmicScreencopyManagerV2,
    output_source_manager: ZcosmicOutputImageSourceManagerV1,
    toplevel_source_manager: ZcosmicToplevelImageSourceManagerV1,
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
                    .iter()
                    .find_map(|wg| {
                        wg.workspaces.iter().find_map(|w| {
                            info.workspace
                                .iter()
                                .any(|x| {
                                    x == &w.handle
                                        && w.state.contains(&WEnum::Value(
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
                        map.entry(o).or_default().push(toplevel.clone());
                    }
                    map
                },
            );

        *self.wayland_helper.inner.toplevels.lock().unwrap() = self
            .toplevel_info_state
            .toplevels()
            .filter_map(|(handle, info)| Some((handle.clone(), info?.clone())))
            .collect();
    }
}

#[derive(Default)]
struct SessionState {
    formats: Option<Formats>,
}

struct SessionInner {
    wayland_helper: WaylandHelper,
    screencopy_session: zcosmic_screencopy_session_v2::ZcosmicScreencopySessionV2,
    condvar: Condvar,
    state: Mutex<SessionState>,
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        self.screencopy_session.destroy();
    }
}

pub struct Session(Arc<SessionInner>);

impl Session {
    pub fn for_session(
        session: &zcosmic_screencopy_session_v2::ZcosmicScreencopySessionV2,
    ) -> Option<Self> {
        session.data::<SessionData>()?.session.upgrade().map(Self)
    }

    fn update<F: FnOnce(&mut SessionState)>(&self, f: F) {
        f(&mut self.0.state.lock().unwrap());
        self.0.condvar.notify_all();
    }

    fn wait_for_formats<T, F: FnMut(&Formats) -> T>(&self, mut cb: F) -> T {
        let data = self
            .0
            .condvar
            .wait_while(self.0.state.lock().unwrap(), |data| data.formats.is_none())
            .unwrap();
        cb(data.formats.as_ref().unwrap())
    }

    /// Capture to `wl_buffer`, blocking until capture either succeeds or fails
    pub async fn capture_wl_buffer(
        &self,
        buffer: &wl_buffer::WlBuffer,
    ) -> Result<Frame, WEnum<zcosmic_screencopy_frame_v2::FailureReason>> {
        let (sender, receiver) = oneshot::channel();
        // TODO damage
        capture(
            &self.0.screencopy_session,
            buffer,
            &[],
            &self.0.wayland_helper.inner.qh,
            FrameData {
                frame_data: Default::default(),
                sender: Mutex::new(Some(sender)),
            },
        );
        self.0.wayland_helper.inner.conn.flush().unwrap();

        // TODO: wait for server to release buffer?
        receiver.await.unwrap()
    }
}

#[derive(Clone, Debug)]
pub enum CaptureSource {
    Output(wl_output::WlOutput),
    Toplevel(ZcosmicToplevelHandleV1),
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
                toplevels: Mutex::new(Vec::new()),
                qh: qh.clone(),
                screencopy_manager: screencopy_state.screencopy_manager.clone(),
                output_source_manager: screencopy_state.output_source_manager.clone().unwrap(),
                toplevel_source_manager: screencopy_state.toplevel_source_manager.clone().unwrap(),
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

    pub fn toplevels(&self) -> Vec<(ZcosmicToplevelHandleV1, ToplevelInfo)> {
        self.inner.toplevels.lock().unwrap().clone()
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

    pub async fn capture_output_toplevels_shm(
        &self,
        output: &wl_output::WlOutput,
        overlay_cursor: bool,
    ) -> Vec<ShmImage<OwnedFd>> {
        // get the active workspace for this output
        // get the toplevels for that workspace
        // capture each toplevel

        let Some(toplevels) = self
            .inner
            .output_toplevels
            .lock()
            .unwrap()
            .get(output)
            .cloned()
        else {
            return Vec::new();
        };

        // TODO is `FuturesOrdered` more optimal?
        let mut images = Vec::new();
        for t in toplevels.into_iter() {
            if let Some(image) = self
                .capture_source_shm(CaptureSource::Toplevel(t), overlay_cursor)
                .await
            {
                images.push(image);
            }
        }
        images
    }

    pub fn capture_source_session(&self, source: CaptureSource, overlay_cursor: bool) -> Session {
        Session(Arc::new_cyclic(|weak_session| {
            let image_source = match source {
                CaptureSource::Output(o) => {
                    self.inner
                        .output_source_manager
                        .create_source(&o, &self.inner.qh, ())
                }
                CaptureSource::Toplevel(t) => {
                    self.inner
                        .toplevel_source_manager
                        .create_source(&t, &self.inner.qh, ())
                }
            };

            let options = if overlay_cursor {
                zcosmic_screencopy_manager_v2::Options::PaintCursors
            } else {
                zcosmic_screencopy_manager_v2::Options::empty()
            };
            let screencopy_session = self.inner.screencopy_manager.create_session(
                &image_source,
                options,
                &self.inner.qh,
                SessionData {
                    session: weak_session.clone(),
                    session_data: Default::default(),
                },
            );

            self.inner.conn.flush().unwrap();

            SessionInner {
                wayland_helper: self.clone(),
                screencopy_session,
                condvar: Condvar::new(),
                state: Default::default(),
            }
        }))
    }

    pub async fn capture_source_shm(
        &self,
        source: CaptureSource,
        overlay_cursor: bool,
    ) -> Option<ShmImage<OwnedFd>> {
        // XXX error type?
        // TODO: way to get cursor metadata?

        let session = self.capture_source_session(source, overlay_cursor);

        // TODO: Check that format has been advertised in `Formats`
        let (width, height) = session.wait_for_formats(|formats| formats.buffer_size);

        let fd = buffer::create_memfd(width, height);
        let buffer =
            self.create_shm_buffer(&fd, width, height, width * 4, wl_shm::Format::Abgr8888);

        let res = session.capture_wl_buffer(&buffer).await;
        buffer.destroy();

        if let Ok(frame) = res {
            let transform = match frame.transform {
                WEnum::Value(value) => value,
                WEnum::Unknown(value) => panic!("invalid capture transform: {}", value),
            };
            Some(ShmImage {
                fd,
                width,
                height,
                transform,
            })
        } else {
            None
        }
    }

    pub fn create_shm_buffer<Fd: AsFd>(
        &self,
        fd: &Fd,
        width: u32,
        height: u32,
        stride: u32,
        format: wl_shm::Format,
    ) -> wl_buffer::WlBuffer {
        let pool = self.inner.wl_shm.create_pool(
            fd.as_fd(),
            stride as i32 * height as i32,
            &self.inner.qh,
            (),
        );
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            format,
            &self.inner.qh,
            (),
        );

        pool.destroy();

        buffer
    }

    pub fn create_dmabuf_buffer<Fd: AsFd>(
        &self,
        dmabuf: &buffer::Dmabuf<Fd>,
    ) -> wl_buffer::WlBuffer {
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
        params.create_immed(
            dmabuf.width as i32,
            dmabuf.height as i32,
            dmabuf.format as u32,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.inner.qh,
            (),
        )
    }
}

pub struct ShmImage<T: AsFd> {
    fd: T,
    pub width: u32,
    pub height: u32,
    pub transform: wl_output::Transform,
}

impl<T: AsFd> ShmImage<T> {
    pub fn image(&self) -> anyhow::Result<image::RgbaImage> {
        let mmap = unsafe { memmap2::Mmap::map(&self.fd.as_fd())? };
        image::RgbaImage::from_raw(self.width, self.height, mmap.to_vec())
            .ok_or_else(|| anyhow::anyhow!("ShmImage had incorrect size"))
    }

    pub fn image_transformed(&self) -> anyhow::Result<image::RgbaImage> {
        let mut image = image::DynamicImage::from(self.image()?);
        image.apply_orientation(match self.transform {
            wl_output::Transform::Normal => image::metadata::Orientation::NoTransforms,
            wl_output::Transform::_90 => image::metadata::Orientation::Rotate90,
            wl_output::Transform::_180 => image::metadata::Orientation::Rotate180,
            wl_output::Transform::_270 => image::metadata::Orientation::Rotate270,
            wl_output::Transform::Flipped => image::metadata::Orientation::FlipHorizontal,
            wl_output::Transform::Flipped90 => image::metadata::Orientation::Rotate90FlipH,
            wl_output::Transform::Flipped180 => image::metadata::Orientation::FlipVertical,
            wl_output::Transform::Flipped270 => image::metadata::Orientation::Rotate270FlipH,
            _ => unreachable!(),
        });
        match image {
            image::DynamicImage::ImageRgba8(image) => Ok(image),
            _ => unreachable!(),
        }
    }
}

impl<T: AsFd + Into<OwnedFd>> From<ShmImage<T>> for Shmbuf {
    fn from(image: ShmImage<T>) -> Self {
        Shmbuf {
            fd: image.fd.into(),
            height: image.height as i32,
            width: image.width as i32,
            offset: 0,
            stride: image.width as i32 * 4,
            // TODO: Change when support for other formats is added
            format: wl_shm::Format::Abgr8888,
        }
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
        session: &zcosmic_screencopy_session_v2::ZcosmicScreencopySessionV2,
        formats: &Formats,
    ) {
        if let Some(session) = Session::for_session(session) {
            session.update(|data| {
                data.formats = Some(formats.clone());
            });
        }
    }

    fn stopped(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _session: &zcosmic_screencopy_session_v2::ZcosmicScreencopySessionV2,
    ) {
        // TODO
    }

    fn ready(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        screencopy_frame: &zcosmic_screencopy_frame_v2::ZcosmicScreencopyFrameV2,
        frame: Frame,
    ) {
        if let Some(sender) = screencopy_frame
            .data::<FrameData>()
            .and_then(|data| data.sender.lock().unwrap().take())
        {
            let _ = sender.send(Ok(frame));
        }
    }

    fn failed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        screencopy_frame: &zcosmic_screencopy_frame_v2::ZcosmicScreencopyFrameV2,
        reason: WEnum<zcosmic_screencopy_frame_v2::FailureReason>,
    ) {
        if let Some(sender) = screencopy_frame
            .data::<FrameData>()
            .and_then(|data| data.sender.lock().unwrap().take())
        {
            let _ = sender.send(Err(reason));
        }
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

fn portal_wayland_socket() -> Option<UnixStream> {
    let fd = std::env::var("PORTAL_WAYLAND_SOCKET")
        .ok()?
        .parse::<RawFd>()
        .ok()?;
    env::remove_var("PORTAL_WAYLAND_SOCKET");
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // set the CLOEXEC flag on this FD
    let mut flags = rustix::io::fcntl_getfd(&fd).ok()?;
    flags.insert(rustix::io::FdFlags::CLOEXEC);
    if let Err(err) = rustix::io::fcntl_setfd(&fd, flags) {
        drop(fd);
        log::error!("Failed to set CLOEXEC on portal socket: {}", err);
        return None;
    }
    Some(UnixStream::from(fd))
}

// Connect to wayland and start task reading events from socket
pub fn connect_to_wayland() -> wayland_client::Connection {
    if let Some(portal_socket) = portal_wayland_socket() {
        wayland_client::Connection::from_socket(portal_socket).unwrap_or_else(|err| {
            log::error!("{}", err);
            process::exit(1)
        })
    } else {
        // Useful fallback for testing and debugging, without `COSMIC_ENABLE_WAYLAND_SECURITY`
        log::warn!("Failed to find `PORTAL_WAYLAND_SOCKET`; trying default Wayland display");
        wayland_client::Connection::connect_to_env().unwrap()
    }
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

struct SessionData {
    session: Weak<SessionInner>,
    session_data: ScreencopySessionData,
}

impl ScreencopySessionDataExt for SessionData {
    fn screencopy_session_data(&self) -> &ScreencopySessionData {
        &self.session_data
    }
}

struct FrameData {
    frame_data: ScreencopyFrameData,
    #[allow(clippy::type_complexity)]
    sender: Mutex<
        Option<oneshot::Sender<Result<Frame, WEnum<zcosmic_screencopy_frame_v2::FailureReason>>>>,
    >,
}

impl ScreencopyFrameDataExt for FrameData {
    fn screencopy_frame_data(&self) -> &ScreencopyFrameData {
        &self.frame_data
    }
}

sctk::delegate_shm!(AppData);
sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_dmabuf!(AppData);
cosmic_client_toolkit::delegate_screencopy!(AppData, session: [SessionData], frame: [FrameData]);
