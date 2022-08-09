// Thread to get frames from compositor and redirect to pipewire

use pipewire::{
    prelude::*,
    spa::{self, pod, utils::Id},
    stream::StreamState,
};
use std::{cell::RefCell, io, rc::Rc};
use tokio::sync::oneshot;

pub struct ScreencastThread {
    node_id: u32,
    thread_stop_tx: pipewire::channel::Sender<()>,
}

impl ScreencastThread {
    pub async fn new() -> anyhow::Result<Self> {
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
        Ok(Self {
            node_id: rx.await.unwrap()?.await.unwrap()?,
            thread_stop_tx,
        })
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    pub fn stop(self) {
        let _ = self.thread_stop_tx.send(());
    }
}

fn start_stream(
) -> Result<(pipewire::MainLoop, oneshot::Receiver<anyhow::Result<u32>>), pipewire::Error> {
    let loop_ = pipewire::MainLoop::new()?;

    let name = format!("cosmic-screenshot"); // XXX randomize?

    let stream_cell: Rc<RefCell<Option<pipewire::stream::Stream<()>>>> =
        Rc::new(RefCell::new(None));
    let stream_cell_clone = stream_cell.clone();

    let (node_id_tx, node_id_rx) = oneshot::channel();
    let node_id_tx = RefCell::new(Some(node_id_tx));

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
                    node_id_tx.send(Ok(stream.node_id())).unwrap();
                }
            }
            StreamState::Error(msg) => {
                if let Some(node_id_tx) = node_id_tx.borrow_mut().take() {
                    node_id_tx
                        .send(Err(anyhow::anyhow!("stream error: {}", msg)))
                        .unwrap();
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
            let datas = buffer.datas_mut();
            let chunk = datas[0].chunk();
            *chunk.size_mut() = 1920 * 1080 * 4;
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 4 * 1920;
            let data = datas[0].get_mut();
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
