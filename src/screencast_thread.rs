// Thread to get frames from compositor and redirect to pipewire
// TODO: Things other than outputs, handle disconnected output, resolution change

// Dmabuf modifier negotiation is described in https://docs.pipewire.org/page_dma_buf.html

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

use crate::{
    buffer::{self, Dmabuf, Plane},
    wayland::{DmabufHelper, WaylandHelper},
};

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
                Ok((loop_, _listener, _context, node_id_rx)) => {
                    tx.send(Ok(node_id_rx)).unwrap();
                    let weak_loop = loop_.downgrade();
                    let _receiver = thread_stop_rx.attach(&loop_, move |()| {
                        weak_loop.upgrade().unwrap().quit();
                    });
                    loop_.run();
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
        pipewire::Context,
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

    let dmabuf_helper = wayland_helper.dmabuf();
    let dmabuf_helper2 = dmabuf_helper.clone(); // XXX
    let dmabuf_helper3 = dmabuf_helper.clone(); // XXX

    // XXX
    // Should use implicit modifier if none set?
    let modifier = Rc::new(RefCell::new(gbm::Modifier::Linear));
    let modifier2 = modifier.clone();
    let modifier3 = modifier.clone();

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
        .param_changed(move |stream, id, (), pod| {
            if id != spa_sys::SPA_PARAM_Format {
                return;
            }
            if let Some(pod) = pod {
                let value = PodDeserializer::deserialize_from::<pod::Value>(pod.as_bytes());
                if let Ok((_, pod::Value::Object(object))) = &value {
                    if let Some(modifier_prop) = object
                        .properties
                        .iter()
                        .find(|p| p.key == spa_sys::SPA_FORMAT_VIDEO_modifier)
                    {
                        if let pod::Value::Choice(pod::ChoiceValue::Long(spa::utils::Choice(
                            _,
                            spa::utils::ChoiceEnum::Enum {
                                default,
                                alternatives,
                            },
                        ))) = &modifier_prop.value
                        {
                            println!(
                                "modifier param-changed: (default: {}, alternatives: {:?})",
                                default, alternatives
                            );
                            if let Ok(modifier_val) = gbm::Modifier::try_from(*default as u64) {
                                *modifier.borrow_mut() = modifier_val;

                                let params = params(
                                    width as u32,
                                    height as u32,
                                    dmabuf_helper3.as_ref(),
                                    Some(modifier_val),
                                );
                                let mut params: Vec<_> = params
                                    .iter()
                                    .map(|x| Pod::from_bytes(x.as_slice()).unwrap())
                                    .collect();
                                stream.update_params(&mut params);
                            }
                        }
                    }
                    //println!("{object:?}");
                }
                //println!("param-changed: {} {:?}", id, value);
            }
        })
        .add_buffer(move |buffer| {
            let buf = unsafe { &mut *(*buffer).buffer };
            let datas = unsafe { slice::from_raw_parts_mut(buf.datas, buf.n_datas as usize) };
            // let metas = unsafe { slice::from_raw_parts(buf.metas, buf.n_metas as usize) };

            // TODO test multi-planar
            println!("type: {}", datas[0].type_);
            if datas[0].type_ & (1 << spa_sys::SPA_DATA_DmaBuf) != 0 {
                println!("Allocate dmabuf buffer");
                let gbm = dmabuf_helper2.as_ref().unwrap().gbm().lock().unwrap();
                let dmabuf = buffer::create_dmabuf(&gbm, *modifier2.borrow(), width, height);

                assert!(dmabuf.planes.len() == datas.len());
                for (data, plane) in datas.iter_mut().zip(dmabuf.planes) {
                    data.type_ = spa_sys::SPA_DATA_DmaBuf;
                    data.flags = 0;
                    data.fd = plane.fd.into_raw_fd() as _;
                    data.data = std::ptr::null_mut();
                    data.maxsize = 0; // XXX
                    data.mapoffset = 0;

                    let chunk = unsafe { &mut *data.chunk };
                    chunk.size = height * plane.stride;
                    chunk.offset = plane.offset;
                    chunk.stride = plane.stride as i32;
                }
            } else {
                println!("Allocate shm buffer");
                for data in datas {
                    let fd = buffer::create_memfd(width, height);

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
                if datas[0].type_() == spa::data::DataType::DmaBuf {
                    let dmabuf = Dmabuf {
                        format: gbm::Format::Abgr8888,
                        modifier: *modifier3.borrow(),
                        width,
                        height,
                        planes: datas
                            .iter()
                            .map(|data| Plane {
                                // TODO avoid dup
                                fd: unsafe { BorrowedFd::borrow_raw(data.as_raw().fd as _) }
                                    .try_clone_to_owned()
                                    .unwrap(),
                                offset: data.chunk().offset(),
                                stride: data.chunk().stride() as u32,
                            })
                            .collect(),
                    };
                    wayland_helper.capture_output_dmabuf_fd(&output, overlay_cursor, &dmabuf);
                } else {
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
            }
        })
        .register()?;
    // DRIVER, ALLOC_BUFFERS

    let params = params(width as u32, height as u32, dmabuf_helper.as_ref(), None);
    let mut params: Vec<_> = params
        .iter()
        .map(|x| Pod::from_bytes(x.as_slice()).unwrap())
        .collect();

    //let flags = pipewire::stream::StreamFlags::MAP_BUFFERS;
    let flags = pipewire::stream::StreamFlags::ALLOC_BUFFERS;
    stream.connect(spa::Direction::Output, None, flags, &mut params)?;
    *stream_cell.borrow_mut() = Some(stream);

    Ok((loop_, listener, context, node_id_rx))
}

fn params(
    width: u32,
    height: u32,
    dmabuf: Option<&DmabufHelper>,
    fixated_modifier: Option<gbm::Modifier>,
) -> Vec<Vec<u8>> {
    [
        Some(buffers(width, height)),
        fixated_modifier.map(|x| format(width, height, None, Some(x))),
        // Favor dmabuf over shm by listing it first
        dmabuf.map(|x| format(width, height, Some(x), None)),
        Some(format(width, height, None, None)),
    ]
    .into_iter()
    .filter_map(|x| x)
    .collect()
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
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_dataType,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Choice(pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Flags {
                        default: 1 << spa_sys::SPA_DATA_DmaBuf, // ?
                        flags: vec![1 << spa_sys::SPA_DATA_MemFd, 1 << spa_sys::SPA_DATA_DmaBuf],
                    },
                ))),
            },
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

// If `dmabuf` is passed, format will be for dmabuf with modifiers
fn format(
    width: u32,
    height: u32,
    dmabuf: Option<&DmabufHelper>,
    fixated_modifier: Option<gbm::Modifier>,
) -> Vec<u8> {
    let mut properties = vec![
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
            value: pod::Value::Id(Id(spa_sys::SPA_VIDEO_FORMAT_RGBA)), // XXX support others?
        },
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
    ];
    if let Some(modifier) = fixated_modifier {
        properties.push(pod::Property {
            key: spa_sys::SPA_FORMAT_VIDEO_modifier,
            flags: pod::PropertyFlags::MANDATORY,
            value: pod::Value::Long(u64::from(modifier) as i64),
        });
    } else if let Some(dmabuf) = dmabuf {
        let mut modifiers: Vec<_> = dmabuf
            .modifiers_for_format(gbm::Format::Abgr8888 as u32)
            .map(|x| x as i64)
            .collect();
        if modifiers.is_empty() {
            // TODO
            modifiers.push(u64::from(gbm::Modifier::Invalid) as _);
        }
        let default = *modifiers.iter().next().unwrap();

        properties.push(pod::Property {
            key: spa_sys::SPA_FORMAT_VIDEO_modifier,
            flags: pod::PropertyFlags::MANDATORY | pod::PropertyFlags::DONT_FIXATE,
            value: pod::Value::Choice(pod::ChoiceValue::Long(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Enum {
                    default,
                    alternatives: modifiers,
                },
            ))),
        });
    }
    value_to_bytes(pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Format,
        id: spa_sys::SPA_PARAM_EnumFormat,
        properties,
    }))
}
