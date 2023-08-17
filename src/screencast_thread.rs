// Thread to get frames from compositor and redirect to pipewire
// TODO: Things other than outputs, handle disconnected output, resolution change

use pipewire::{
    spa::{
        self,
        pod::{self, deserialize::PodDeserializer, serialize::PodSerializer, Pod},
        utils::Id,
    },
    stream::StreamState,
};
use std::{
    cell::RefCell,
    io,
    os::fd::{BorrowedFd, IntoRawFd},
    rc::Rc,
    slice,
};
use tokio::sync::oneshot;
use wayland_client::protocol::wl_output;

use crate::wayland::WaylandHelper;

pub struct ScreencastThread {
    node_id: u32,
    thread_stop_tx: pipewire::channel::Sender<()>,
}

impl ScreencastThread {
    pub async fn new(
        wayland_helper: WaylandHelper,
        output: wl_output::WlOutput,
        overlay_cursor: bool,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = oneshot::channel();
        let (thread_stop_tx, thread_stop_rx) = pipewire::channel::channel::<()>();
        std::thread::spawn(
            move || match start_stream(wayland_helper, output, overlay_cursor) {
                Ok((loop_, listener, context, node_id_rx)) => {
                    tx.send(Ok(node_id_rx)).unwrap();
                    let weak_loop = loop_.downgrade();
                    let _receiver = thread_stop_rx.attach(&loop_, move |()| {
                        weak_loop.upgrade().unwrap().quit();
                    });
                    loop_.run();
                    // XXX fix segfault with opposite drop order
                    drop(listener);
                    drop(context);
                }
                Err(err) => tx.send(Err(err)).unwrap(),
            },
        );
        Ok(Self {
            // XXX can second unwrap fail?
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
    wayland_helper: WaylandHelper,
    output: wl_output::WlOutput,
    overlay_cursor: bool,
) -> Result<
    (
        pipewire::MainLoop,
        pipewire::stream::StreamListener<()>,
        pipewire::Context<pipewire::MainLoop>,
        oneshot::Receiver<anyhow::Result<u32>>,
    ),
    pipewire::Error,
> {
    let loop_ = pipewire::MainLoop::new()?;
    let context = pipewire::Context::new(&loop_)?;
    let core = context.connect(None)?;

    let name = format!("cosmic-screenshot"); // XXX randomize?

    let stream_cell: Rc<RefCell<Option<pipewire::stream::Stream>>> = Rc::new(RefCell::new(None));
    let stream_cell_clone = stream_cell.clone();

    let (node_id_tx, node_id_rx) = oneshot::channel();
    let mut node_id_tx = Some(node_id_tx);

    let (width, height) = match wayland_helper.capture_output_shm(&output, overlay_cursor) {
        Some(frame) => (frame.width, frame.height),
        None => (0, 0), // XXX
    };

    let stream = pipewire::stream::Stream::new(
        &core,
        &name,
        pipewire::properties! {
            "media.class" => "Video/Source",
            "node.name" => "cosmic-screenshot", // XXX
        },
    )?;
    let listener = stream
        .add_local_listener()
        .state_changed(move |old, new| {
            println!("state-changed '{:?}' -> '{:?}'", old, new);
            match new {
                StreamState::Paused => {
                    let stream = stream_cell_clone.borrow_mut();
                    let stream = stream.as_ref().unwrap();
                    if let Some(node_id_tx) = node_id_tx.take() {
                        node_id_tx.send(Ok(stream.node_id())).unwrap();
                    }
                }
                StreamState::Error(msg) => {
                    if let Some(node_id_tx) = node_id_tx.take() {
                        node_id_tx
                            .send(Err(anyhow::anyhow!("stream error: {}", msg)))
                            .unwrap();
                    }
                }
                _ => {}
            }
        })
        .param_changed(|_, id, (), pod| {
            if id != spa_sys::SPA_PARAM_Format {
                return;
            }
            if let Some(pod) = pod {
                let value = PodDeserializer::deserialize_from::<pod::Value>(pod.as_bytes());
                println!("param-changed: {} {:?}", id, value);
            }
        })
        .add_buffer(move |buffer| {
            let buf = unsafe { &mut *(*buffer).buffer };
            let datas = unsafe { slice::from_raw_parts_mut(buf.datas, buf.n_datas as usize) };
            // let metas = unsafe { slice::from_raw_parts(buf.metas, buf.n_metas as usize) };

            for data in datas {
                use std::ffi::CStr;

                let name = unsafe { CStr::from_bytes_with_nul_unchecked(b"pipewire-screencopy\0") };
                let fd = rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC).unwrap(); // XXX
                rustix::fs::ftruncate(&fd, (width * height * 4) as _);

                // TODO test `data.type_`

                data.type_ = spa_sys::SPA_DATA_MemFd;
                data.flags = 0;
                data.fd = fd.into_raw_fd() as _;
                data.data = std::ptr::null_mut();
                data.maxsize = width * height * 4;
                data.mapoffset = 0;

                let chunk = unsafe { &mut *data.chunk };
                chunk.size = width * height * 4;
                chunk.offset = 0;
                chunk.stride = 4 * width as i32;
            }
        })
        .remove_buffer(|buffer| {
            let buf = unsafe { &mut *(*buffer).buffer };
            let datas = unsafe { slice::from_raw_parts_mut(buf.datas, buf.n_datas as usize) };

            for data in datas {
                let _ = unsafe { rustix::io::close(data.fd as _) };
                data.fd = -1;
            }
        })
        .process(move |stream, ()| {
            if let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                //let data = datas[0].get_mut();
                //if data.len() == width as usize * height as usize * 4 {
                let fd = unsafe { BorrowedFd::borrow_raw(datas[0].as_raw().fd as _) };
                // TODO error
                wayland_helper.capture_output_shm_fd(
                    &output,
                    overlay_cursor,
                    fd,
                    Some(width * height * 4),
                );
                //if frame.width == width && frame.height == height {
                //}
            }
        })
        .register()?;
    // DRIVER, ALLOC_BUFFERS
    // ??? define formats (shm, dmabuf)
    let format = format(width, height);
    let buffers = buffers(width as u32, height as u32);
    let params = &mut [
        Pod::from_bytes(buffers.as_slice()).unwrap(),
        Pod::from_bytes(format.as_slice()).unwrap(),
    ];
    //let flags = pipewire::stream::StreamFlags::MAP_BUFFERS;
    let flags = pipewire::stream::StreamFlags::ALLOC_BUFFERS;
    println!("not connected {}", stream.node_id());
    stream.connect(spa::Direction::Output, None, flags, params)?;
    println!("connected? {}", stream.node_id());
    *stream_cell.borrow_mut() = Some(stream);

    Ok((loop_, listener, context, node_id_rx))
}

fn value_to_bytes(value: pod::Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut cursor = io::Cursor::new(&mut bytes);
    PodSerializer::serialize(&mut cursor, &value).unwrap();
    bytes
}

fn buffers(width: u32, height: u32) -> Vec<u8> {
    value_to_bytes(pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa_sys::SPA_PARAM_Buffers,
        properties: vec![
            /*
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_dataType,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Choice(pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Flags {
                        default: 1 << spa_sys::SPA_DATA_MemFd,
                        flags: vec![],
                    },
                ))),
            },
            */
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_size,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Int(width as i32 * height as i32 * 4),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_stride,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Int(width as i32 * 4),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_align,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Int(16),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_blocks,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Int(1),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_buffers,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Choice(pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Range {
                        default: 4,
                        min: 1,
                        max: 32,
                    },
                ))),
            },
        ],
    }))
}

fn format(width: u32, height: u32) -> Vec<u8> {
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
                value: pod::Value::Rectangle(spa::utils::Rectangle { width, height }),
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
