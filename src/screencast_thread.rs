// Thread to get frames from compositor and redirect to pipewire
// TODO: Things other than outputs, handle disconnected output, resolution change
// TODO use `buffer_infos` to determine supported modifiers, formats

// Dmabuf modifier negotiation is described in https://docs.pipewire.org/page_dma_buf.html

use cosmic_client_toolkit::screencopy::Rect;
use futures::executor::block_on;
use pipewire::{
    spa::{
        self,
        pod::{self, deserialize::PodDeserializer, serialize::PodSerializer, Pod},
        utils::Id,
    },
    stream::{StreamRef, StreamState},
    sys::pw_buffer,
};
use spa_sys::{spa_format_video_raw_parse, spa_video_info_raw};

use crate::{
    buffer,
    wayland::{CaptureSource, DmabufHelper, Session, WaylandHelper},
};
use std::{
    collections::HashMap,
    ffi::c_void,
    io, iter,
    os::fd::IntoRawFd,
    slice,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use wayland_client::{
    protocol::{wl_buffer, wl_output, wl_shm},
    WEnum,
};

const TIMESPEC_NSEC_PER_SEC: u32 = 1_000_000_000;
const FPS_MEASURE_PERIOD_SEC: f64 = 5.;

pub struct ScreencastThread {
    node_id: u32,
    thread_stop_tx: pipewire::channel::Sender<()>,
}

impl ScreencastThread {
    pub async fn new(
        wayland_helper: WaylandHelper,
        capture_source: CaptureSource,
        overlay_cursor: bool,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = oneshot::channel();
        let (thread_stop_tx, thread_stop_rx) = pipewire::channel::channel::<()>();

        std::thread::spawn(move || {
            match start_stream(wayland_helper, capture_source, overlay_cursor) {
                Ok((loop_, _stream, _listener, _context, node_id_rx)) => {
                    tx.send(Ok(node_id_rx)).unwrap();
                    let weak_loop = loop_.downgrade();
                    let _receiver = thread_stop_rx.attach(loop_.loop_(), move |()| {
                        weak_loop.upgrade().unwrap().quit();
                    });
                    loop_.run();
                }
                Err(err) => tx.send(Err(err)).unwrap(),
            }
        });

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
struct FPSLimit {
    frame_last_time: Instant,
    fps_last_time: Instant,
    fps_frame_count: u32,
    delay_before_capture_frame_ns: u64,
    delay_til_next_frame_ns: u64,
    accumulated_frame_debt_ns: u64,
}

impl FPSLimit {
    fn new() -> Self {
        Self {
            frame_last_time: Instant::now(),
            fps_last_time: Instant::now(),
            fps_frame_count: 0,
            delay_before_capture_frame_ns: 0,
            delay_til_next_frame_ns: 0,
            accumulated_frame_debt_ns: 0,
        }
    }

    fn fps_limit_measure_start(&mut self, max_fps: u32) {
        if max_fps <= 0 {
            return;
        }

        self.frame_last_time = Instant::now();
    }

    fn measure_fps(&mut self) {
        let now = Instant::now();
        self.fps_frame_count += 1;
        let elapsed_sec = (now - self.fps_last_time).as_secs_f64();

        if elapsed_sec < FPS_MEASURE_PERIOD_SEC {
            return;
        }
        let avg_frames_per_sec = self.fps_frame_count as f64 / elapsed_sec;

        log::info!(
            "fps_limit: average FPS in the last {:.2} seconds: {:.2}",
            elapsed_sec,
            avg_frames_per_sec
        );
        self.fps_frame_count = 0;
        self.fps_last_time = now;
    }

    fn fps_limit_measure_end(&mut self, max_fps: u32) {
        if max_fps <= 0 {
            self.delay_before_capture_frame_ns = 0;
            self.delay_til_next_frame_ns = 0;
            self.accumulated_frame_debt_ns = 0;
            return;
        }
        self.measure_fps();

        let elapsed_ns = self.frame_last_time.elapsed().as_nanos();
        let target_ns = (TIMESPEC_NSEC_PER_SEC / max_fps) as u128;

        // Wait for half of the target frame rate duration before requesting a frame capture.
        self.delay_before_capture_frame_ns = (target_ns / 2) as u64;

        // Throttle after the current frame has been captured:
        let total_elapsed_ns = elapsed_ns + self.accumulated_frame_debt_ns as u128;
        if target_ns > total_elapsed_ns {
            // If it is before the next frame capture time -> wait for the right time.
            self.delay_til_next_frame_ns = (target_ns - total_elapsed_ns) as u64;
        } else {
            // If it is after the next frame capture time, Set value of `delay_til_next_frame_ns` to 0 and increase value of `accumulated_frame_debt_ns` by the amount of time it has been delayed.
            self.delay_til_next_frame_ns = 0;
            self.accumulated_frame_debt_ns = target_ns.abs_diff(total_elapsed_ns) as u64;
        }

        // Set `delay_before_capture_frame_ns` to its current value minus the overrun time, if any.
        if self.accumulated_frame_debt_ns > self.delay_before_capture_frame_ns {
            self.accumulated_frame_debt_ns -= self.delay_before_capture_frame_ns;
            self.delay_before_capture_frame_ns = 0;
        } else {
            self.delay_before_capture_frame_ns -= self.accumulated_frame_debt_ns;
            self.accumulated_frame_debt_ns = 0;
        }

        // Reset at the end of each capture cycle, this helps prevent `accumulated_frame_debt_ns` from increasing indefinitely.
        if self.fps_frame_count % max_fps == 0 {
            self.accumulated_frame_debt_ns = 0;
        }
    }
}

struct StreamData {
    dmabuf_helper: Option<DmabufHelper>,
    wayland_helper: WaylandHelper,
    modifier: gbm::Modifier,
    session: Session,
    width: u32,
    height: u32,
    node_id_tx: Option<oneshot::Sender<Result<u32, anyhow::Error>>>,
    buffer_damage: HashMap<wl_buffer::WlBuffer, Vec<Rect>>,
    // fps limit
    framerate: u32,
    fps_limit: FPSLimit,
    fps_max: u32,
}

impl StreamData {
    // Get driver preferred modifier, and plane count
    fn choose_modifier(&self, modifiers: &[gbm::Modifier]) -> Option<(gbm::Modifier, u32)> {
        let gbm = self.dmabuf_helper.as_ref().unwrap().gbm().lock().unwrap();
        if modifiers.iter().all(|x| *x == gbm::Modifier::Invalid) {
            match gbm.create_buffer_object::<()>(
                self.width,
                self.height,
                gbm::Format::Abgr8888,
                gbm::BufferObjectFlags::empty(),
            ) {
                Ok(bo) => Some((gbm::Modifier::Invalid, bo.plane_count())),
                Err(err) => {
                    log::error!(
                        "Failed to choose modifier by creating temporary bo: {}",
                        err
                    );
                    None
                }
            }
        } else {
            match gbm.create_buffer_object_with_modifiers2::<()>(
                self.width,
                self.height,
                gbm::Format::Abgr8888,
                modifiers.iter().copied(),
                gbm::BufferObjectFlags::empty(),
            ) {
                Ok(bo) => Some((bo.modifier(), bo.plane_count())),
                Err(err) => {
                    log::error!(
                        "Failed to choose modifier by creating temporary bo: {}",
                        err
                    );
                    None
                }
            }
        }
    }

    fn state_changed(&mut self, stream: &StreamRef, old: StreamState, new: StreamState) {
        log::info!("state-changed '{:?}' -> '{:?}'", old, new);
        match new {
            StreamState::Paused => {
                if let Some(node_id_tx) = self.node_id_tx.take() {
                    node_id_tx.send(Ok(stream.node_id())).unwrap();
                }
            }
            StreamState::Error(msg) => {
                if let Some(node_id_tx) = self.node_id_tx.take() {
                    node_id_tx
                        .send(Err(anyhow::anyhow!("stream error: {}", msg)))
                        .unwrap();
                }
            }
            _ => {}
        }
    }

    fn param_changed(&mut self, stream: &StreamRef, id: u32, pod: Option<&Pod>) {
        if id != spa_sys::SPA_PARAM_Format {
            return;
        }
        if let Some(pod) = pod {
            let value = PodDeserializer::deserialize_from::<pod::Value>(pod.as_bytes());
            if let Ok((_, pod::Value::Object(object))) = &value {
                let pwr_format: spa_video_info_raw = unsafe {
                    let mut pwr_format = std::mem::MaybeUninit::<spa_video_info_raw>::uninit();
                    spa_format_video_raw_parse(
                        pod.as_raw_ptr() as *const _,
                        pwr_format.as_mut_ptr(),
                    );
                    pwr_format.assume_init()
                };
                if pwr_format.max_framerate.denom != 0 {
                    let framerate = pwr_format.max_framerate.num / pwr_format.max_framerate.denom;
                    self.framerate = if framerate > self.fps_max {
                        self.fps_max
                    } else {
                        framerate
                    };
                }

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
                        log::info!(
                            "modifier param-changed: (default: {}, alternatives: {:?})",
                            default,
                            alternatives
                        );

                        // Create temporary bo to get preferred modifier
                        // Similar to xdg-desktop-portal-wlr
                        let modifiers = iter::once(default)
                            .chain(alternatives)
                            .map(|x| gbm::Modifier::from(*x as u64))
                            .collect::<Vec<_>>();
                        if let Some((modifier, plane_count)) = self.choose_modifier(&modifiers) {
                            self.modifier = modifier;

                            let params = params(
                                self.width,
                                self.height,
                                self.framerate,
                                plane_count,
                                self.dmabuf_helper.as_ref(),
                                Some(modifier),
                            );
                            let mut params: Vec<_> = params
                                .iter()
                                .map(|x| Pod::from_bytes(x.as_slice()).unwrap())
                                .collect();
                            if let Err(err) = stream.update_params(&mut params) {
                                log::error!("failed to update pipewire params: {}", err);
                            }
                        }
                    }
                }
            }
            //println!("param-changed: {} {:?}", id, value);
        }
    }

    fn add_buffer(&mut self, _stream: &StreamRef, buffer: *mut pw_buffer) {
        let buf = unsafe { &mut *(*buffer).buffer };
        let datas = unsafe { slice::from_raw_parts_mut(buf.datas, buf.n_datas as usize) };
        // let metas = unsafe { slice::from_raw_parts(buf.metas, buf.n_metas as usize) };

        let wl_buffer;
        if datas[0].type_ & (1 << spa_sys::SPA_DATA_DmaBuf) != 0 {
            log::info!("Allocate dmabuf buffer");
            let gbm = self.dmabuf_helper.as_ref().unwrap().gbm().lock().unwrap();
            let dmabuf = buffer::create_dmabuf(&gbm, self.modifier, self.width, self.height);

            wl_buffer = self.wayland_helper.create_dmabuf_buffer(&dmabuf);

            assert!(dmabuf.planes.len() == datas.len());
            for (data, plane) in datas.iter_mut().zip(dmabuf.planes) {
                data.type_ = spa_sys::SPA_DATA_DmaBuf;
                data.flags = 0;
                data.fd = plane.fd.into_raw_fd() as _;
                data.data = std::ptr::null_mut();
                data.maxsize = 0; // XXX
                data.mapoffset = 0;

                let chunk = unsafe { &mut *data.chunk };
                chunk.size = self.height * plane.stride;
                chunk.offset = plane.offset;
                chunk.stride = plane.stride as i32;
            }
        } else {
            log::info!("Allocate shm buffer");
            assert_eq!(datas.len(), 1);
            let data = &mut datas[0];

            let fd = buffer::create_memfd(self.width, self.height);

            wl_buffer = self.wayland_helper.create_shm_buffer(
                &fd,
                self.width,
                self.height,
                self.width * 4,
                wl_shm::Format::Abgr8888,
            );

            data.type_ = spa_sys::SPA_DATA_MemFd;
            data.flags = 0;
            data.fd = fd.into_raw_fd() as _;
            data.data = std::ptr::null_mut();
            data.maxsize = self.width * self.height * 4;
            data.mapoffset = 0;

            let chunk = unsafe { &mut *data.chunk };
            chunk.size = self.width * self.height * 4;
            chunk.offset = 0;
            chunk.stride = 4 * self.width as i32;
        }

        let user_data = Box::into_raw(Box::new(wl_buffer)) as *mut c_void;
        unsafe { (*buffer).user_data = user_data };
    }

    fn remove_buffer(&mut self, _stream: &StreamRef, buffer: *mut pw_buffer) {
        let buf = unsafe { &mut *(*buffer).buffer };
        let datas = unsafe { slice::from_raw_parts_mut(buf.datas, buf.n_datas as usize) };

        for data in datas {
            unsafe { rustix::io::close(data.fd as _) };
            data.fd = -1;
        }

        let wl_buffer: Box<wl_buffer::WlBuffer> =
            unsafe { Box::from_raw((*buffer).user_data as *mut _) };
        self.buffer_damage.remove(&*wl_buffer);
        wl_buffer.destroy();
    }

    fn process(&mut self, stream: &StreamRef) {
        let buffer = unsafe { stream.dequeue_raw_buffer() };
        if !buffer.is_null() {
            self.fps_limit.fps_limit_measure_start(self.framerate);
            if self.fps_limit.delay_before_capture_frame_ns != 0 {
                // log::info!(
                //     "fps_limit: wait {}ns before capture frame",
                //     self.fps_limit.delay_before_capture_frame_ns
                // );
                std::thread::sleep(Duration::from_nanos(
                    self.fps_limit.delay_before_capture_frame_ns,
                ));
            }
            let wl_buffer = unsafe { &*((*buffer).user_data as *const wl_buffer::WlBuffer) };
            let full_damage = &[Rect {
                x: 0,
                y: 0,
                width: self.width as i32,
                height: self.height as i32,
            }];
            let damage = self
                .buffer_damage
                .get(wl_buffer)
                .map(Vec::as_slice)
                .unwrap_or(full_damage);

            match block_on(self.session.capture_wl_buffer(wl_buffer, damage)) {
                Ok(frame) => {
                    self.buffer_damage
                        .entry(wl_buffer.clone())
                        .or_default()
                        .clear();
                    for (b, damage) in self.buffer_damage.iter_mut() {
                        if b != wl_buffer {
                            damage.extend_from_slice(&frame.damage);
                        }
                    }
                    if let Some(video_transform) = unsafe {
                        buffer_find_meta_data::<spa_sys::spa_meta_videotransform>(
                            buffer,
                            spa_sys::SPA_META_VideoTransform,
                        )
                    } {
                        video_transform.transform = convert_transform(frame.transform);
                    }
                }
                Err(err) => {
                    log::error!("screencopy failed: {:?}", err);
                    // TODO terminate screencasting?
                }
            }
            unsafe { stream.queue_raw_buffer(buffer) };
            self.fps_limit.fps_limit_measure_end(self.framerate);

            if self.fps_limit.delay_til_next_frame_ns != 0 {
                // log::info!(
                //     "fps_limit: wait {}ns til next frame",
                //     self.fps_limit.delay_til_next_frame_ns
                // );
                std::thread::sleep(Duration::from_nanos(self.fps_limit.delay_til_next_frame_ns));
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn start_stream(
    wayland_helper: WaylandHelper,
    capture_source: CaptureSource,
    overlay_cursor: bool,
) -> anyhow::Result<(
    pipewire::main_loop::MainLoop,
    pipewire::stream::Stream,
    pipewire::stream::StreamListener<StreamData>,
    pipewire::context::Context,
    oneshot::Receiver<anyhow::Result<u32>>,
)> {
    let loop_ = pipewire::main_loop::MainLoop::new(None)?;
    let context = pipewire::context::Context::new(&loop_)?;
    let core = context.connect(None)?;

    let name = "cosmic-screenshot".to_string(); // XXX randomize?

    let (node_id_tx, node_id_rx) = oneshot::channel();

    let framerate = 0; // default not limit the frame rate.

    let session = wayland_helper.capture_source_session(capture_source, overlay_cursor);

    let Some((width, height)) = block_on(session.wait_for_formats(|formats| formats.buffer_size))
    else {
        return Err(anyhow::anyhow!(
            "failed to get formats for image copy; session stopped"
        ));
    };

    let dmabuf_helper = wayland_helper.dmabuf();

    let stream = pipewire::stream::Stream::new(
        &core,
        &name,
        pipewire::properties::properties! {
            "media.class" => "Video/Source",
            "node.name" => "cosmic-screenshot", // XXX
        },
    )?;

    let initial_params = params(width, height, framerate, 1, dmabuf_helper.as_ref(), None);
    let mut initial_params: Vec<_> = initial_params
        .iter()
        .map(|x| Pod::from_bytes(x.as_slice()).unwrap())
        .collect();

    //let flags = pipewire::stream::StreamFlags::MAP_BUFFERS;
    let flags = pipewire::stream::StreamFlags::ALLOC_BUFFERS;
    stream.connect(
        spa::utils::Direction::Output,
        None,
        flags,
        &mut initial_params,
    )?;

    let data = StreamData {
        wayland_helper,
        dmabuf_helper,
        session,
        // XXX Should use implicit modifier if none set?
        modifier: gbm::Modifier::Linear,
        width,
        height,
        node_id_tx: Some(node_id_tx),
        buffer_damage: HashMap::new(),
        framerate,
        fps_limit: FPSLimit::new(),
        fps_max: 120, // XXX can read from config?
    };

    let listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|stream, data, old, new| data.state_changed(stream, old, new))
        .param_changed(|stream, data, id, pod| data.param_changed(stream, id, pod))
        .add_buffer(|stream, data, buffer| data.add_buffer(stream, buffer))
        .remove_buffer(|stream, data, buffer| data.remove_buffer(stream, buffer))
        .process(|stream, data| data.process(stream))
        .register()?;

    Ok((loop_, stream, listener, context, node_id_rx))
}

fn convert_transform(transform: WEnum<wl_output::Transform>) -> u32 {
    match transform {
        WEnum::Value(wl_output::Transform::Normal) => spa_sys::SPA_META_TRANSFORMATION_None,
        WEnum::Value(wl_output::Transform::_90) => spa_sys::SPA_META_TRANSFORMATION_90,
        WEnum::Value(wl_output::Transform::_180) => spa_sys::SPA_META_TRANSFORMATION_180,
        WEnum::Value(wl_output::Transform::_270) => spa_sys::SPA_META_TRANSFORMATION_270,
        WEnum::Value(wl_output::Transform::Flipped) => spa_sys::SPA_META_TRANSFORMATION_Flipped,
        WEnum::Value(wl_output::Transform::Flipped90) => spa_sys::SPA_META_TRANSFORMATION_Flipped90,
        WEnum::Value(wl_output::Transform::Flipped180) => {
            spa_sys::SPA_META_TRANSFORMATION_Flipped180
        }
        WEnum::Value(wl_output::Transform::Flipped270) => {
            spa_sys::SPA_META_TRANSFORMATION_Flipped270
        }
        WEnum::Value(_) | WEnum::Unknown(_) => unreachable!(),
    }
}

// SAFETY: buffer must be non-null, and valid as long as return value is used
unsafe fn buffer_find_meta_data<'a, T>(
    buffer: *const pipewire_sys::pw_buffer,
    type_: u32,
) -> Option<&'a mut T> {
    let ptr = spa_sys::spa_buffer_find_meta_data((*buffer).buffer, type_, size_of::<T>());
    (ptr as *mut T).as_mut()
}

fn meta() -> Vec<u8> {
    value_to_bytes(pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa_sys::SPA_PARAM_Meta,
        properties: vec![
            pod::Property {
                key: spa_sys::SPA_PARAM_META_type,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Id(spa::utils::Id(spa_sys::SPA_META_VideoTransform)),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_META_size,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Int(size_of::<spa_sys::spa_meta_videotransform>() as _),
            },
        ],
    }))
    // TODO: header, video damage
}

fn params(
    width: u32,
    height: u32,
    framerate: u32,
    blocks: u32,
    dmabuf: Option<&DmabufHelper>,
    fixated_modifier: Option<gbm::Modifier>,
) -> Vec<Vec<u8>> {
    [
        Some(buffers(width, height, blocks)),
        fixated_modifier.map(|x| format(width, height, framerate, None, Some(x))),
        // Favor dmabuf over shm by listing it first
        dmabuf.map(|x| format(width, height, framerate, Some(x), None)),
        Some(format(width, height, framerate, None, None)),
        Some(meta()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn value_to_bytes(value: pod::Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut cursor = io::Cursor::new(&mut bytes);
    PodSerializer::serialize(&mut cursor, &value).unwrap();
    bytes
}

fn buffers(width: u32, height: u32, blocks: u32) -> Vec<u8> {
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
                value: pod::Value::Int(blocks as i32),
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
    framerate: u32,
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
            value: pod::Value::Fraction(spa::utils::Fraction { num: 0, denom: 1 }),
        },
        // TODO max framerate
    ];
    if framerate > 0 {
        properties.push(pod::Property {
            key: spa_sys::SPA_FORMAT_VIDEO_maxFramerate,
            flags: pod::PropertyFlags::empty(),
            value: pod::Value::Choice(pod::ChoiceValue::Fraction(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Range {
                    default: spa::utils::Fraction {
                        num: framerate,
                        denom: 1,
                    },
                    min: spa::utils::Fraction { num: 1, denom: 1 },
                    max: spa::utils::Fraction {
                        num: framerate,
                        denom: 1,
                    },
                },
            ))),
        });
    }
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
        let default = *modifiers.first().unwrap();

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
