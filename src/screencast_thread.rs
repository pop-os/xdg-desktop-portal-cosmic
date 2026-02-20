// Thread to get frames from compositor and redirect to pipewire
// TODO: Things other than outputs, handle disconnected output, resolution change
// TODO use `buffer_infos` to determine supported modifiers, formats

// Dmabuf modifier negotiation is described in https://docs.pipewire.org/page_dma_buf.html

use cosmic_client_toolkit::screencopy::{FailureReason, Formats, Rect};
use futures::{executor::block_on, stream::StreamExt};
use pipewire::{
    spa::{
        self,
        pod::{self, Pod, deserialize::PodDeserializer, serialize::PodSerializer},
        utils::Id,
    },
    stream::{Stream, StreamState},
    sys::pw_buffer,
};
use std::{
    collections::HashMap,
    ffi::c_void,
    io, iter,
    os::fd::IntoRawFd,
    slice,
    task::{Context, Poll, Waker},
};
use tokio::sync::oneshot;
use wayland_client::{
    WEnum,
    protocol::{wl_buffer, wl_output, wl_shm},
};

use crate::{
    buffer,
    screencast::StreamProps,
    wayland::{CaptureSource, CursorStream, DmabufHelper, Session, WaylandHelper},
};

static FORMAT_MAP: &[(gbm::Format, Id)] = &[
    (gbm::Format::Abgr8888, Id(spa_sys::SPA_VIDEO_FORMAT_RGBA)),
    (gbm::Format::Argb8888, Id(spa_sys::SPA_VIDEO_FORMAT_BGRA)),
];

fn spa_format(format: gbm::Format) -> Option<Id> {
    Some(FORMAT_MAP.iter().find(|(f, _)| *f == format)?.1)
}

fn spa_format_to_gbm(format: Id) -> Option<gbm::Format> {
    Some(FORMAT_MAP.iter().find(|(_, f)| *f == format)?.0)
}

fn shm_format(format: gbm::Format) -> Option<wl_shm::Format> {
    match format {
        gbm::Format::Argb8888 => Some(wl_shm::Format::Argb8888),
        gbm::Format::Xrgb8888 => Some(wl_shm::Format::Xrgb8888),
        _ => wl_shm::Format::try_from(format as u32).ok(),
    }
}

fn shm_format_to_gbm(format: wl_shm::Format) -> Option<gbm::Format> {
    match format {
        wl_shm::Format::Argb8888 => Some(gbm::Format::Argb8888),
        wl_shm::Format::Xrgb8888 => Some(gbm::Format::Xrgb8888),
        _ => gbm::Format::try_from(format as u32).ok(),
    }
}

#[repr(C)]
struct MetadataCursor {
    meta_cursor: spa_sys::spa_meta_cursor,
    meta_bitmap: spa_sys::spa_meta_bitmap,
    bytes: [u8],
}

impl MetadataCursor {
    pub fn size_of(width: u32, height: u32) -> usize {
        std::mem::offset_of!(Self, meta_bitmap)
            + std::mem::size_of::<spa_sys::spa_meta_bitmap>()
            + width as usize * height as usize * 4
    }

    // , image: &crate::wayland::ShmImage<std::os::fd::OwnedFd>
    fn update(&mut self, image: Option<&image::RgbaImage>, position: (i32, i32)) {
        self.meta_cursor = spa_sys::spa_meta_cursor {
            id: 1,
            flags: 0,
            position: spa_sys::spa_point {
                x: position.0,
                y: position.1,
            },
            hotspot: spa_sys::spa_point { x: 0, y: 0 },
            //bitmap_offset: 0,
            bitmap_offset: std::mem::offset_of!(Self, meta_bitmap) as u32,
        };
        let Some(image) = image else {
            self.meta_cursor.bitmap_offset = 0;
            return;
        };
        self.meta_bitmap = spa_sys::spa_meta_bitmap {
            format: spa_sys::SPA_VIDEO_FORMAT_RGBA,
            size: spa_sys::spa_rectangle {
                // XXX
                width: image.width(),
                height: image.height(),
            },
            stride: image.width() as i32 * 4,
            offset: std::mem::size_of::<spa_sys::spa_meta_bitmap>() as u32,
        };
        // XXX what if buffer is not large enough?
        self.bytes[..image.len()].copy_from_slice(image);
    }
}

pub struct ScreencastThread {
    stream_props: StreamProps,
    node_id: u32,
    thread_stop_tx: pipewire::channel::Sender<()>,
}

impl ScreencastThread {
    pub async fn new(
        wayland_helper: WaylandHelper,
        capture_source: CaptureSource,
        overlay_cursor: bool,
        stream_props: StreamProps,
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
            stream_props,
            // XXX can second unwrap fail?
            node_id: rx.await.unwrap()?.await.unwrap()?,
            thread_stop_tx,
        })
    }

    pub fn stream_props(&self) -> StreamProps {
        self.stream_props.clone()
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    pub fn stop(self) {
        let _ = self.thread_stop_tx.send(());
    }
}

struct StreamData {
    dmabuf_helper: Option<DmabufHelper>,
    wayland_helper: WaylandHelper,
    format: gbm::Format,
    modifier: Option<gbm::Modifier>,
    session: Session,
    cursor_stream: Option<CursorStream>,
    cursor_image: Option<image::RgbaImage>,
    formats: Formats,
    node_id_tx: Option<oneshot::Sender<Result<u32, anyhow::Error>>>,
    buffer_damage: HashMap<wl_buffer::WlBuffer, Vec<Rect>>,
    update_cursor: bool,
}

impl StreamData {
    fn width(&self) -> u32 {
        self.formats.buffer_size.0
    }

    fn height(&self) -> u32 {
        self.formats.buffer_size.1
    }

    fn plane_count(&self, format: gbm::Format, modifier: gbm::Modifier) -> Option<u32> {
        let dmabuf_helper = self.dmabuf_helper.as_ref().unwrap();
        let mut gbm_devices = dmabuf_helper.gbm_devices().lock().unwrap();
        let dev = self
            .formats
            .dmabuf_device
            .unwrap_or(dmabuf_helper.feedback().main_device());
        let (_, gbm) = gbm_devices.gbm_device(dev).ok()??;
        gbm.format_modifier_plane_count(format, modifier)
    }

    // Get driver preferred modifier, and plane count
    fn choose_modifier(
        &self,
        format: gbm::Format,
        modifiers: &[gbm::Modifier],
    ) -> Option<gbm::Modifier> {
        let dmabuf_helper = self.dmabuf_helper.as_ref().unwrap();
        let mut gbm_devices = dmabuf_helper.gbm_devices().lock().unwrap();
        let dev = self
            .formats
            .dmabuf_device
            .unwrap_or(dmabuf_helper.feedback().main_device());
        let gbm = match gbm_devices.gbm_device(dev) {
            Ok(Some((_, gbm))) => gbm,
            Ok(None) => {
                log::error!("Failed to find gbm device for '{dev}'");
                return None;
            }
            Err(err) => {
                log::error!("Failed to open gbm device for '{dev}': {err}");
                return None;
            }
        };
        if modifiers.iter().all(|x| *x == gbm::Modifier::Invalid) {
            match gbm.create_buffer_object::<()>(
                self.width(),
                self.height(),
                format,
                gbm::BufferObjectFlags::empty(),
            ) {
                Ok(_bo) => Some(gbm::Modifier::Invalid),
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
                self.width(),
                self.height(),
                format,
                modifiers.iter().copied(),
                gbm::BufferObjectFlags::empty(),
            ) {
                Ok(bo) => Some(bo.modifier()),
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

    // Handle changes to capture source size, etc.
    fn update_formats(&mut self, stream: &Stream) -> bool {
        let Some(formats) = block_on(self.session.wait_for_formats(|formats| formats.clone()))
        else {
            return false;
        };

        if formats == self.formats {
            // No change to formats, so nothing to do.
            return false;
        }

        let initial_params = format_params(self.dmabuf_helper.as_ref(), None, &formats);
        let mut initial_params: Vec<_> = initial_params.iter().map(|x| &**x).collect();
        if let Err(err) = stream.update_params(&mut initial_params) {
            log::error!("failed to update pipewire params: {}", err);
        }

        self.formats = formats;

        true
    }

    fn state_changed(&mut self, stream: &Stream, old: StreamState, new: StreamState) {
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

    fn param_changed(&mut self, stream: &Stream, id: u32, pod: Option<&Pod>) {
        let Some(pod) = pod else {
            return;
        };
        if id != spa_sys::SPA_PARAM_Format {
            return;
        }
        let object = match pod.as_object() {
            Ok(object) => object,
            Err(err) => {
                log::error!("format param not an object?: {}", err);
                return;
            }
        };

        let mut pwr_format = spa::param::video::VideoInfoRaw::new();
        if let Err(err) = pwr_format.parse(pod) {
            log::error!("error parsing pipewire video info: {}", err);
        }

        self.format = if let Some(gbm_format) = spa_format_to_gbm(Id(pwr_format.format().0)) {
            gbm_format
        } else {
            log::error!("pipewire format not recognized: {:?}", pwr_format);
            return;
        };

        if let Some(modifier_prop) = object.find_prop(Id(spa_sys::SPA_FORMAT_VIDEO_modifier)) {
            let value =
                PodDeserializer::deserialize_from::<pod::Value>(modifier_prop.value().as_bytes());
            let Ok((_, pod::Value::Choice(pod::ChoiceValue::Long(spa::utils::Choice(_, choice))))) =
                &value
            else {
                log::error!("invalid modifier prop: {:?}", value);
                return;
            };

            if modifier_prop
                .flags()
                .contains(pod::PodPropFlags::DONT_FIXATE)
            {
                let spa::utils::ChoiceEnum::Enum {
                    default,
                    alternatives,
                } = choice
                else {
                    // TODO How does C code deal with variants of choice?
                    log::error!("invalid modifier prop choice: {:?}", choice);
                    return;
                };

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
                if let Some(modifier) = self.choose_modifier(self.format, &modifiers) {
                    self.modifier = Some(modifier);

                    let params = format_params(
                        self.dmabuf_helper.as_ref(),
                        Some((self.format, modifier)),
                        &self.formats,
                    );
                    let mut params: Vec<_> = params.iter().map(|x| &**x).collect();
                    if let Err(err) = stream.update_params(&mut params) {
                        log::error!("failed to update pipewire params: {}", err);
                    }
                    return;
                } else {
                    log::error!("failed to choose modifier from {:?}", modifiers);
                    let params = format_params(None, None, &self.formats);
                    let mut params: Vec<_> = params.iter().map(|x| &**x).collect();
                    if let Err(err) = stream.update_params(&mut params) {
                        log::error!("failed to update pipewire params: {}", err);
                    }
                    return;
                }
            }
        }

        log::info!("modifier fixated. Setting other params.");

        let blocks = self
            .modifier
            .and_then(|m| self.plane_count(self.format, m))
            .unwrap_or(1);
        let params = other_params(self.width(), self.height(), blocks, self.modifier.is_some());
        let mut params: Vec<_> = params.iter().map(|x| &**x).collect();
        if let Err(err) = stream.update_params(&mut params) {
            log::error!("failed to update pipewire params: {}", err);
        }
    }

    fn add_buffer(&mut self, _stream: &Stream, buffer: *mut pw_buffer) {
        let buf = unsafe { &mut *(*buffer).buffer };
        let datas = unsafe { slice::from_raw_parts_mut(buf.datas, buf.n_datas as usize) };
        // let metas = unsafe { slice::from_raw_parts(buf.metas, buf.n_metas as usize) };

        let wl_buffer;
        if datas[0].type_ & (1 << spa_sys::SPA_DATA_DmaBuf) != 0 {
            log::info!("Allocate dmabuf buffer");
            let dmabuf_helper = self.dmabuf_helper.as_ref().unwrap();
            let mut gbm_devices = dmabuf_helper.gbm_devices().lock().unwrap();
            let dev = self
                .formats
                .dmabuf_device
                .unwrap_or(dmabuf_helper.feedback().main_device());
            // Unwrap: assumes `choose_buffer` successfully opened gbm device
            let (_, gbm) = gbm_devices.gbm_device(dev).unwrap().unwrap();
            let dmabuf = buffer::create_dmabuf(
                gbm,
                self.format,
                self.modifier.unwrap(),
                self.width(),
                self.height(),
            );

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
                chunk.size = self.height() * plane.stride;
                chunk.offset = plane.offset;
                chunk.stride = plane.stride as i32;
            }
        } else {
            log::info!("Allocate shm buffer");
            assert_eq!(datas.len(), 1);
            let data = &mut datas[0];

            let fd = buffer::create_memfd(self.width(), self.height());

            wl_buffer = self.wayland_helper.create_shm_buffer(
                &fd,
                self.width(),
                self.height(),
                self.width() * 4,
                shm_format(self.format).unwrap(),
            );

            data.type_ = spa_sys::SPA_DATA_MemFd;
            data.flags = spa_sys::SPA_DATA_FLAG_READABLE | spa_sys::SPA_DATA_FLAG_MAPPABLE;
            data.fd = fd.into_raw_fd() as _;
            data.data = std::ptr::null_mut();
            data.maxsize = self.width() * self.height() * 4;
            data.mapoffset = 0;

            let chunk = unsafe { &mut *data.chunk };
            chunk.size = self.width() * self.height() * 4;
            chunk.offset = 0;
            chunk.stride = 4 * self.width() as i32;
        }

        let user_data = Box::into_raw(Box::new(wl_buffer)) as *mut c_void;
        unsafe { (*buffer).user_data = user_data };
    }

    fn remove_buffer(&mut self, _stream: &Stream, buffer: *mut pw_buffer) {
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

    fn process(&mut self, stream: &Stream) {
        if let Some(stream) = &mut self.cursor_stream {
            let mut context = Context::from_waker(Waker::noop());
            if let Poll::Ready(image) = stream.poll_next_unpin(&mut context) {
                println!("Have cursor image");
                self.cursor_image = image;
            }
        }

        let buffer = unsafe { stream.dequeue_raw_buffer() };
        if !buffer.is_null() {
            let wl_buffer = unsafe { &*((*buffer).user_data as *const wl_buffer::WlBuffer) };
            let full_damage = &[Rect {
                x: 0,
                y: 0,
                width: self.width() as i32,
                height: self.height() as i32,
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
                    if let Some(cursor) = unsafe {
                        buffer_cursor_find_meta_data(
                            buffer,
                            spa_sys::SPA_META_Cursor,
                            MetadataCursor::size_of(64, 64),
                        )
                    } {
                        let position = self.session.cursor_position();
                        cursor.update(self.cursor_image.as_ref(), position);
                        self.update_cursor = false;
                    }
                }
                Err(err) => {
                    if err == WEnum::Value(FailureReason::BufferConstraints) {
                        let changed = self.update_formats(stream);
                        if !changed {
                            log::error!("screencopy buffer constraints error, but no new formats?");
                        }
                    } else {
                        log::error!("screencopy failed: {:?}", err);
                        // TODO terminate screencasting?
                    }
                }
            }
            unsafe { stream.queue_raw_buffer(buffer) };
        }
    }
}

#[allow(clippy::type_complexity)]
fn start_stream(
    wayland_helper: WaylandHelper,
    capture_source: CaptureSource,
    overlay_cursor: bool,
) -> anyhow::Result<(
    pipewire::main_loop::MainLoopRc,
    pipewire::stream::StreamRc,
    pipewire::stream::StreamListener<StreamData>,
    pipewire::context::ContextRc,
    oneshot::Receiver<anyhow::Result<u32>>,
)> {
    let loop_ = pipewire::main_loop::MainLoopRc::new(None)?;
    let context = pipewire::context::ContextRc::new(&loop_, None)?;
    let core = context.connect_rc(None)?;

    let name = "cosmic-screenshot".to_string(); // XXX randomize?

    let (node_id_tx, node_id_rx) = oneshot::channel();

    let session = wayland_helper.capture_source_session(capture_source, overlay_cursor);
    let mut cursor_stream = session.cursor_stream();
    let mut cursor_image = None;
    if let Some(stream) = &mut cursor_stream {
        let mut context = Context::from_waker(Waker::noop());
        if let Poll::Ready(image) = stream.poll_next_unpin(&mut context) {
            println!("Have cursor image 0");
            cursor_image = image;
        }
    }

    // TODO initial poll?

    let Some(formats) = block_on(session.wait_for_formats(|formats| formats.clone())) else {
        return Err(anyhow::anyhow!(
            "failed to get formats for image copy; session stopped"
        ));
    };

    let dmabuf_helper = wayland_helper.dmabuf();

    let stream = pipewire::stream::StreamRc::new(
        core,
        &name,
        pipewire::properties::properties! {
            "media.class" => "Video/Source",
            "node.name" => "cosmic-screenshot", // XXX
        },
    )?;

    let initial_params = format_params(dmabuf_helper.as_ref(), None, &formats);
    let mut initial_params: Vec<_> = initial_params.iter().map(|x| &**x).collect();

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
        cursor_stream,
        cursor_image,
        formats,
        format: gbm::Format::Abgr8888,
        modifier: None,
        node_id_tx: Some(node_id_tx),
        buffer_damage: HashMap::new(),
        update_cursor: true,
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

// SAFETY: buffer must be non-null, valid as long as return value is used
//unsafe fn buffer_find_meta_data_with_size<'a, T: ?Sized>(
/*
unsafe fn buffer_find_meta_data_with_size<'a, T: ?Sized>(
    buffer: *const pipewire_sys::pw_buffer,
    type_: u32,
    size: usize,
) -> Option<&'a mut T> {
    let ptr = spa_sys::spa_buffer_find_meta_data((*buffer).buffer, type_, size);
    (std::ptr::slice_from_raw_parts(ptr, size) as *mut T).as_mut()
}
*/

unsafe fn buffer_cursor_find_meta_data<'a>(
    buffer: *const pipewire_sys::pw_buffer,
    type_: u32,
    size: usize,
) -> Option<&'a mut MetadataCursor> {
    let ptr = spa_sys::spa_buffer_find_meta_data((*buffer).buffer, type_, size);
    (std::ptr::slice_from_raw_parts(ptr, size) as *mut MetadataCursor).as_mut()
}

// SAFETY: buffer must be non-null, and valid as long as return value is used
unsafe fn buffer_find_meta_data<'a, T>(
    buffer: *const pipewire_sys::pw_buffer,
    type_: u32,
) -> Option<&'a mut T> {
    unsafe {
        let ptr = spa_sys::spa_buffer_find_meta_data((*buffer).buffer, type_, size_of::<T>());
        (ptr as *mut T).as_mut()
    }
}

struct OwnedPod(Vec<u8>);

impl OwnedPod {
    fn new(content: Vec<u8>) -> Self {
        assert!(Pod::from_bytes(&content).is_some());
        Self(content)
    }

    fn serialize(value: &pod::Value) -> Self {
        let mut bytes = Vec::new();
        let mut cursor = io::Cursor::new(&mut bytes);
        PodSerializer::serialize(&mut cursor, value).unwrap();
        Self::new(bytes)
    }
}

impl std::ops::Deref for OwnedPod {
    type Target = Pod;

    fn deref(&self) -> &Pod {
        // Unchecked version of `Pod::from_bytes`
        unsafe { Pod::from_raw(self.0.as_ptr().cast()) }
    }
}

fn meta() -> OwnedPod {
    OwnedPod::serialize(&pod::Value::Object(pod::Object {
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

fn meta_cursor() -> OwnedPod {
    OwnedPod::serialize(&pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa_sys::SPA_PARAM_Meta,
        properties: vec![
            pod::Property {
                key: spa_sys::SPA_PARAM_META_type,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Id(spa::utils::Id(spa_sys::SPA_META_Cursor)),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_META_size,
                flags: pod::PropertyFlags::empty(),
                // XXX
                value: pod::Value::Int(MetadataCursor::size_of(64, 64) as _),
            },
        ],
    }))
}

fn format_params(
    dmabuf: Option<&DmabufHelper>,
    fixated: Option<(gbm::Format, gbm::Modifier)>,
    formats: &Formats,
) -> Vec<OwnedPod> {
    let (width, height) = formats.buffer_size;

    let mut pods = Vec::new();
    if let Some((fixated_format, fixated_modifier)) = fixated {
        pods.extend(format(
            width,
            height,
            None,
            fixated_format,
            Some(fixated_modifier),
            formats,
        ));
    }
    // Favor dmabuf over shm by listing it first
    if let Some(dmabuf) = dmabuf {
        for (gbm_format, _) in &formats.dmabuf_formats {
            if let Ok(gbm_format) = gbm::Format::try_from(*gbm_format) {
                pods.extend(format(
                    width,
                    height,
                    Some(dmabuf),
                    gbm_format,
                    None,
                    formats,
                ));
            }
        }
    }
    for shm_format in &formats.shm_formats {
        if let Some(gbm_format) = shm_format_to_gbm(*shm_format) {
            pods.extend(format(width, height, None, gbm_format, None, formats));
        }
    }
    pods
}

fn other_params(width: u32, height: u32, blocks: u32, allow_dmabuf: bool) -> Vec<OwnedPod> {
    [
        Some(buffers(width, height, blocks, allow_dmabuf)),
        Some(meta()),
        Some(meta_cursor()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn buffers(width: u32, height: u32, blocks: u32, allow_dmabuf: bool) -> OwnedPod {
    OwnedPod::serialize(&pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamBuffers,
        id: spa_sys::SPA_PARAM_Buffers,
        properties: vec![
            pod::Property {
                key: spa_sys::SPA_PARAM_BUFFERS_dataType,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Choice(pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    if allow_dmabuf {
                        spa::utils::ChoiceEnum::Flags {
                            default: 1 << spa_sys::SPA_DATA_DmaBuf, // ?
                            flags: vec![
                                1 << spa_sys::SPA_DATA_MemFd,
                                1 << spa_sys::SPA_DATA_DmaBuf,
                            ],
                        }
                    } else {
                        spa::utils::ChoiceEnum::Flags {
                            default: 1 << spa_sys::SPA_DATA_MemFd,
                            flags: vec![1 << spa_sys::SPA_DATA_MemFd],
                        }
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
    dmabuf: Option<&DmabufHelper>,
    format: gbm::Format,
    fixated_modifier: Option<gbm::Modifier>,
    formats: &Formats,
) -> Option<OwnedPod> {
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
            value: pod::Value::Id(spa_format(format)?),
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
    } else if let Some(_dmabuf) = dmabuf {
        // TODO: Support other formats
        let modifiers = formats
            .dmabuf_formats
            .iter()
            .find(|(x, _)| *x == format as u32)
            .map(|(_, modifiers)| modifiers.as_slice())
            .unwrap_or_default();
        let modifiers = modifiers
            .iter()
            // Don't allow implict modifiers, which don't work well with multi-GPU
            // TODO: If needed for anything, allow this but only on single-GPU system
            .filter(|m| **m != u64::from(gbm::Modifier::Invalid))
            .map(|x| *x as i64)
            .collect::<Vec<_>>();

        let default = modifiers.first().copied()?;

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
    Some(OwnedPod::serialize(&pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_Format,
        id: spa_sys::SPA_PARAM_EnumFormat,
        properties,
    })))
}
