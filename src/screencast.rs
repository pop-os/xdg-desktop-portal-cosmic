use pipewire::{
    prelude::*,
    spa::{self, pod, utils::Id},
    stream::StreamState,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    io,
    rc::Rc,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use zbus::zvariant;

const CURSOR_MODE_HIDDEN: u32 = 1;
const CURSOR_MODE_EMBEDDED: u32 = 2;
const CURSOR_MODE_METADATA: u32 = 4;

const SOURCE_TYPE_MONITOR: u32 = 1;
const SOURCE_TYPE_WINDOW: u32 = 2;
const SOURCE_TYPE_VIRTUAL: u32 = 4;

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct CreateSessionResult {
    session_id: String,
}

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SelectSourcesOptions {
    // Default: monitor
    types: Option<u32>,
    // Default: false
    multiple: Option<bool>,
    restore_data: Option<(String, u32, zvariant::OwnedValue)>,
    // Default: 0
    persist_mode: Option<u32>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    streams: Vec<(u32, HashMap<String, zvariant::OwnedValue>)>,
    persist_mode: Option<u32>,
    restore_data: Option<(String, u32, zvariant::OwnedValue)>,
}

#[derive(Default)]
struct SessionData {
    thread_stop_tx: Option<pipewire::channel::Sender<()>>,
    closed: bool,
}

impl SessionData {
    fn close(&mut self) {
        if let Some(thread_stop_tx) = self.thread_stop_tx.take() {
            let _ = thread_stop_tx.send(());
        }
        self.closed = true
        // XXX Remove from hashmap?
    }
}

#[derive(Default)]
pub struct ScreenCast {
    sessions: Mutex<HashMap<zvariant::ObjectPath<'static>, Arc<Mutex<SessionData>>>>,
}

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    async fn create_session(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> (u32, CreateSessionResult) {
        // TODO: handle
        let session_data = Arc::new(Mutex::new(SessionData::default()));
        self.sessions
            .lock()
            .unwrap()
            .insert(session_handle.to_owned(), session_data.clone());
        let destroy_session = move || session_data.lock().unwrap().close();
        connection
            .object_server()
            .at(&session_handle, crate::Session::new(destroy_session))
            .await
            .unwrap(); // XXX unwrap
        (
            crate::PORTAL_RESPONSE_SUCCESS,
            CreateSessionResult {
                session_id: "foo".to_string(), // XXX
            },
        )
    }

    async fn select_sources(
        &self,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: SelectSourcesOptions,
    ) -> (u32, HashMap<String, zvariant::OwnedValue>) {
        // TODO: XXX
        (crate::PORTAL_RESPONSE_SUCCESS, HashMap::new())
    }

    async fn start(
        &self,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> (u32, StartResult) {
        let session_data = match self.sessions.lock().unwrap().get(&session_handle) {
            Some(session_data) => session_data.clone(),
            None => {
                return (
                    crate::PORTAL_RESPONSE_OTHER,
                    StartResult {
                        streams: vec![],
                        persist_mode: None,
                        restore_data: None,
                    },
                )
            }
        };

        let res = start_stream_on_thread().await;

        let (res, streams) = if let Ok((Some(node_id), thread_stop_tx)) = res {
            let mut session_data = session_data.lock().unwrap();
            session_data.thread_stop_tx = Some(thread_stop_tx);
            if session_data.closed {
                session_data.close();
                (crate::PORTAL_RESPONSE_OTHER, vec![])
            } else {
                (
                    crate::PORTAL_RESPONSE_SUCCESS,
                    vec![(node_id, HashMap::new())],
                )
            }
        } else {
            (crate::PORTAL_RESPONSE_OTHER, vec![])
        };
        (
            res,
            StartResult {
                // XXX
                streams,
                persist_mode: None,
                restore_data: None,
            },
        )
    }

    #[dbus_interface(property)]
    async fn available_source_types(&self) -> u32 {
        // XXX
        SOURCE_TYPE_MONITOR
    }

    #[dbus_interface(property)]
    async fn available_cursor_modes(&self) -> u32 {
        // XXX
        CURSOR_MODE_HIDDEN
    }

    #[dbus_interface(property, name = "version")]
    async fn version(&self) -> u32 {
        4
    }
}

async fn start_stream_on_thread(
) -> Result<(Option<u32>, pipewire::channel::Sender<()>), pipewire::Error> {
    let (tx, rx) = oneshot::channel();
    let (thread_stop_tx, thread_stop_rx) = pipewire::channel::channel::<()>();
    std::thread::spawn(move || match start_stream() {
        Ok((loop_, node_id_rx)) => {
            tx.send(Ok(node_id_rx)).unwrap();
            let weak_loop = loop_.downgrade();
            let _receiver = thread_stop_rx.attach(&loop_, move |()| {
                weak_loop.upgrade().unwrap().quit();
            });
            loop_.run();
        }
        Err(err) => tx.send(Err(err)).unwrap(),
    });
    Ok((rx.await.unwrap()?.await.unwrap(), thread_stop_tx))
}

fn start_stream() -> Result<(pipewire::MainLoop, oneshot::Receiver<Option<u32>>), pipewire::Error> {
    let loop_ = pipewire::MainLoop::new()?;

    let name = format!("cosmic-screenshot"); // XXX randomize?

    let stream_cell: Rc<RefCell<Option<pipewire::stream::Stream<()>>>> =
        Rc::new(RefCell::new(None));
    let stream_cell_clone = stream_cell.clone();

    let (node_id_tx, node_id_rx) = oneshot::channel();
    let mut node_id_tx = RefCell::new(Some(node_id_tx));

    let stream = pipewire::stream::Stream::with_user_data(
        &loop_,
        &name,
        pipewire::properties! {
            "media.class" => "Video/Source",
            "node.name" => "cosmic-screenshot", // XXX
        },
        (),
    )
    .state_changed(move |old, new| {
        println!("state-changed '{:?}' -> '{:?}'", old, new);
        match new {
            StreamState::Paused => {
                let stream = stream_cell_clone.borrow_mut();
                let stream = stream.as_ref().unwrap();
                if let Some(node_id_tx) = node_id_tx.borrow_mut().take() {
                    node_id_tx.send(Some(stream.node_id())).unwrap();
                }
            }
            // XXX err?
            StreamState::Error(_) => {
                if let Some(node_id_tx) = node_id_tx.borrow_mut().take() {
                    node_id_tx.send(None).unwrap();
                }
            }
            _ => {}
        }
    })
    .param_changed(|id, (), pod| {
        if id != spa_sys::SPA_PARAM_Format {
            return;
        }
        if let Some(pod) = std::ptr::NonNull::new(pod as *mut _) {
            let value = unsafe {
                spa::pod::deserialize::PodDeserializer::deserialize_ptr::<pod::Value>(pod)
            };
            println!("param-changed: {} {:?}", id, value);
        }
    })
    .process(|stream, ()| {
        if let Some(mut buffer) = stream.dequeue_buffer() {
            let mut datas = buffer.datas_mut();
            let mut chunk = datas[0].chunk();
            *chunk.size_mut() = 1920 * 1080 * 4;
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 4 * 1920;
            let mut data = datas[0].get_mut();
            if data.len() == 1920 * 1080 * 4 {
                for i in 0..(1920 * 1080) {
                    data[i * 4] = 255;
                    data[i * 4 + 1] = 0;
                    data[i * 4 + 2] = 0;
                    data[i * 4 + 3] = 255;
                }
            }
        }
    })
    .create()?;
    // DRIVER, ALLOC_BUFFERS
    // ??? define formats (shm, dmabuf)
    let format = format();
    let buffers = buffers();
    let params = &mut [
        buffers.as_slice() as *const _ as _,
        format.as_slice() as *const _ as _,
    ];
    let flags = pipewire::stream::StreamFlags::MAP_BUFFERS;
    stream.connect(spa::Direction::Output, None, flags, params)?;
    *stream_cell.borrow_mut() = Some(stream);

    Ok((loop_, node_id_rx))
}

fn value_to_bytes(value: pod::Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut cursor = io::Cursor::new(&mut bytes);
    spa::pod::serialize::PodSerializer::serialize(&mut cursor, &value).unwrap();
    bytes
}

fn buffers() -> Vec<u8> {
    value_to_bytes(pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa_sys::SPA_PARAM_Buffers,
        properties: vec![
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_size,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Int(1920 * 1080 * 4),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_stride,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Int(1920 * 4),
            },
        ],
    }))
}

fn format() -> Vec<u8> {
    value_to_bytes(pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Format,
        id: spa_sys::SPA_PARAM_EnumFormat,
        properties: vec![
            pod::Property {
                key: spa_sys::SPA_FORMAT_mediaType,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Id(Id(spa_sys::SPA_MEDIA_TYPE_video)),
            },
            pod::Property {
                key: spa_sys::SPA_FORMAT_mediaSubtype,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Id(Id(spa_sys::SPA_MEDIA_SUBTYPE_raw)),
            },
            pod::Property {
                key: spa_sys::SPA_FORMAT_VIDEO_format,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Id(Id(spa_sys::SPA_VIDEO_FORMAT_RGBA)),
            },
            // XXX modifiers
            pod::Property {
                key: spa_sys::SPA_FORMAT_VIDEO_size,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Rectangle(spa::utils::Rectangle {
                    width: 1920,
                    height: 1080,
                }),
            },
            pod::Property {
                key: spa_sys::SPA_FORMAT_VIDEO_framerate,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Fraction(spa::utils::Fraction { num: 60, denom: 1 }),
            },
            // TODO max framerate
        ],
    }))
}
