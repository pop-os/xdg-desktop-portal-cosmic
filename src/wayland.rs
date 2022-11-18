#![allow(unused_variables)]

use cosmic_client_toolkit::{
    cosmic_protocols::screencopy::v1::client::{
        zcosmic_screencopy_manager_v1, zcosmic_screencopy_session_v1,
    },
    screencopy::{BufferInfo, ScreencopyHandler, ScreencopyState},
    sctk::{
        self,
        error::GlobalError,
        globals::ProvidesBoundGlobal,
        output::{OutputHandler, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
        shm::{raw::RawPool, ShmHandler, ShmState},
    },
};
use std::{
    collections::HashMap,
    io::Write,
    process,
    sync::{mpsc, Arc, Condvar, Mutex, MutexGuard},
    thread,
};
use wayland_client::{
    backend::ObjectId,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm},
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};

// TODO
#[derive(Clone)]
struct BoundGlobal<T: wayland_client::Proxy, const V: u32>(T);

impl<I: wayland_client::Proxy, const V: u32> ProvidesBoundGlobal<I, V> for BoundGlobal<I, V> {
    fn bound_global(&self) -> Result<I, GlobalError> {
        Ok(self.0.clone())
    }
}

struct WaylandHelperInner {
    conn: wayland_client::Connection,
    outputs: Mutex<Vec<wl_output::WlOutput>>,
    qh: QueueHandle<AppData>,
    screencopy_manager: zcosmic_screencopy_manager_v1::ZcosmicScreencopyManagerV1,
    wl_shm: BoundGlobal<wl_shm::WlShm, 1>,
    sessions: Mutex<HashMap<ObjectId, Session>>,
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
    shm_state: ShmState,
}

#[derive(Default)]
struct SessionData {
    buffer_infos: Option<Vec<BufferInfo>>,
    res: Option<Result<(), WEnum<zcosmic_screencopy_session_v1::FailureReason>>>,
}

// TODO: dmabuf? need to handle modifier negotation
#[derive(Default)]
struct SessionInner {
    condvar: Condvar,
    data: Mutex<SessionData>,
}

#[derive(Clone, Default)]
struct Session {
    inner: Arc<SessionInner>,
}

impl Session {
    fn update<F: FnOnce(&mut SessionData)>(&self, f: F) {
        f(&mut *self.inner.data.lock().unwrap());
        self.inner.condvar.notify_all();
    }

    fn wait_while<F: FnMut(&SessionData) -> bool>(&self, mut f: F) -> MutexGuard<SessionData> {
        self.inner
            .condvar
            .wait_while(self.inner.data.lock().unwrap(), |data| f(data))
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
        let shm_state = ShmState::bind(&globals, &qh).unwrap();
        let wayland_helper = WaylandHelper {
            inner: Arc::new(WaylandHelperInner {
                conn,
                outputs: Mutex::new(Vec::new()),
                qh: qh.clone(),
                screencopy_manager: screencopy_state.screencopy_manager.clone(),
                wl_shm: BoundGlobal(shm_state.wl_shm().clone()),
                sessions: Mutex::new(HashMap::new()),
            }),
        };
        let mut data = AppData {
            shm_state,
            wayland_helper: wayland_helper.clone(),
            output_state: OutputState::new(&globals, &qh),
            screencopy_state,
            registry_state,
        };
        event_queue.flush().unwrap();

        event_queue.roundtrip(&mut data).unwrap();

        thread::spawn(move || loop {
            event_queue.blocking_dispatch(&mut data).unwrap();
        });

        wayland_helper
    }

    pub fn outputs(&self) -> Vec<wl_output::WlOutput> {
        // TODO Good way to avoid allocation?
        self.inner.outputs.lock().unwrap().clone()
    }

    pub fn capture_output_shm(
        &self,
        output: &wl_output::WlOutput,
        overlay_cursor: bool,
    ) -> Result<ShmImage, std::io::Error> {
        // TODO: way to get cursor metadata?

        let overlay_cursor = if overlay_cursor { 1 } else { 0 };

        let mut sessions = self.inner.sessions.lock().unwrap();
        let screencopy_session = self.inner.screencopy_manager.capture_output(
            &output,
            zcosmic_screencopy_manager_v1::CursorMode::Hidden, // XXX take into account adventised capabilities
            &self.inner.qh,
            Default::default(),
        );
        self.inner.conn.flush().unwrap();
        let session = Session::default();
        sessions.insert(screencopy_session.id(), session.clone());
        drop(sessions);

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

        // Can only assume ARGB8888. Try RGBA8888, but fallback and rotate components?
        // If cosmic-comp specific, can assume support for whatever compositor supports.
        let mut pool = RawPool::new(
            buffer_info.height as usize * buffer_info.width as usize * 4,
            &self.inner.wl_shm,
        )
        .unwrap();
        let buffer = pool.create_buffer(
            0,
            buffer_info.width as i32,
            buffer_info.height as i32,
            buffer_info.stride as i32,
            wl_shm::Format::Abgr8888,
            (),
            &self.inner.qh,
        ); // XXX RGBA

        screencopy_session.attach_buffer(&buffer, None, 0); // XXX age?
        screencopy_session.commit(zcosmic_screencopy_session_v1::Options::empty());
        self.inner.conn.flush().unwrap();

        // TODO: wait for server to release buffer?
        let _res = session
            .wait_while(|data| data.res.is_none())
            .res
            .take()
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(16));

        Ok(ShmImage {
            pool,
            buffer,
            width: 2560,
            height: 1440,
        })
    }
}

pub struct ShmImage {
    pool: RawPool,
    buffer: wl_buffer::WlBuffer,
    pub width: u32,
    pub height: u32,
}

impl ShmImage {
    pub fn bytes(&mut self) -> &[u8] {
        &*self.pool.mmap()
    }

    pub fn write_to_png<T: Write>(&mut self, file: T) -> anyhow::Result<()> {
        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(self.bytes())?;

        Ok(())
    }
}

impl Drop for ShmImage {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    sctk::registry_handlers!(OutputState);
}

impl ShmHandler for AppData {
    fn shm_state(&mut self) -> &mut ShmState {
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
        conn: &Connection,
        qh: &QueueHandle<Self>,
        session: &zcosmic_screencopy_session_v1::ZcosmicScreencopySessionV1,
        buffer_infos: &[BufferInfo],
    ) {
        let sessions = self.wayland_helper.inner.sessions.lock().unwrap();
        sessions.get(&session.id()).unwrap().update(|data| {
            data.buffer_infos = Some(buffer_infos.to_vec());
        });
    }

    fn ready(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        session: &zcosmic_screencopy_session_v1::ZcosmicScreencopySessionV1,
    ) {
        let sessions = self.wayland_helper.inner.sessions.lock().unwrap();
        sessions.get(&session.id()).unwrap().update(|data| {
            data.res = Some(Ok(()));
        });
    }

    fn failed(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        session: &zcosmic_screencopy_session_v1::ZcosmicScreencopySessionV1,
        reason: WEnum<zcosmic_screencopy_session_v1::FailureReason>,
    ) {
        // TODO send message to thread
        let sessions = self.wayland_helper.inner.sessions.lock().unwrap();
        sessions.get(&session.id()).unwrap().update(|data| {
            data.res = Some(Err(reason));
        });
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
    let wayland_connection = match wayland_client::Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("Error: {}", err);
            process::exit(1)
        }
    };

    wayland_connection
}

sctk::delegate_shm!(AppData);
sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
cosmic_client_toolkit::delegate_screencopy!(AppData);
