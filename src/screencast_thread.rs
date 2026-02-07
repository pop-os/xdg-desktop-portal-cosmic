// Thread to get frames from compositor and redirect to pipewire
// TODO: Things other than outputs, handle disconnected output, resolution change
// TODO use `buffer_infos` to determine supported modifiers, formats

// Dmabuf modifier negotiation is described in https://docs.pipewire.org/page_dma_buf.html

use cosmic_client_toolkit::screencopy::{FailureReason, Formats, Frame, Rect};
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
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::c_void,
    io, iter,
    os::fd::IntoRawFd,
    pin::Pin,
    ptr::{self, NonNull},
    rc::Rc,
    slice,
    task::{Context, Poll, Waker},
    thread_local,
    time::{Duration, Instant},
};
use tokio::{sync::oneshot, time};
use wayland_client::{
    WEnum,
    protocol::{wl_buffer, wl_output, wl_shm},
};

use crate::{
    buffer,
    screencast::StreamProps,
    wayland::{CaptureSource, DmabufHelper, Session, WaylandHelper},
};

const USEC_PER_SEC: u64 = 1_000_000;
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

#[derive(Debug)]
enum StreamEvent {
    MinFrameInterval(u64),
    Streaming,
    Paused,
    Error,
}

struct PwBufferUserData {
    wl_buffer: wl_buffer::WlBuffer,
    is_attached_with_pw_buffer: Cell<bool>,
}

impl Drop for PwBufferUserData {
    fn drop(&mut self) {
        self.wl_buffer.destroy();
    }
}

/// # Safety
///
/// `PwBuffer` is not guaranteed.
/// The `raw_buffer` MUST eventually be queued back to the PipeWire stream via `queue_raw_buffer`,
/// otherwise the buffer resource will leak and PipeWire may out of buffer.
struct PwBuffer {
    raw_buffer: NonNull<pw_buffer>,
    user_data: Rc<PwBufferUserData>,
    frame_size: (u32, u32),
    timestamp: Instant,
}

impl PwBuffer {
    /// # Safety
    ///
    /// `PwBuffer` is not guaranteed.
    /// The `raw_buffer` MUST eventually be queued back to the PipeWire stream via `queue_raw_buffer`,
    /// otherwise the buffer resource will leak and PipeWire may out of buffer.
    pub unsafe fn from_raw_buffer(raw_buffer: NonNull<pw_buffer>, frame_size: (u32, u32)) -> Self {
        let data = unsafe {
            let frame_data_ptr = (raw_buffer.as_ref()).user_data as *const PwBufferUserData;
            Rc::increment_strong_count(frame_data_ptr);
            Rc::from_raw(frame_data_ptr)
        };

        Self {
            raw_buffer,
            user_data: data,
            frame_size,
            timestamp: Instant::now(),
        }
    }
}

struct BufferSlotInner {
    buffer: Option<PwBuffer>,
    waker: Option<Waker>,
}

struct BufferSlot {
    inner: Rc<RefCell<BufferSlotInner>>,
}

impl BufferSlot {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(BufferSlotInner {
                buffer: None,
                waker: None,
            })),
        }
    }

    pub fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }

    pub fn set(&self, buffer: PwBuffer) {
        let mut inner = self.inner.borrow_mut();
        inner.buffer = Some(buffer);
        if let Some(waker) = inner.waker.take() {
            waker.wake();
        }
    }
}

impl Future for BufferSlot {
    type Output = PwBuffer;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.inner.borrow_mut();
        match inner.buffer.take() {
            Some(buffer) => Poll::Ready(buffer),
            None => {
                inner.waker = Some(_cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

thread_local! {
    static FRAME_CAPTURE_RESULT: Cell<*mut Option<(PwBuffer, Result<Frame, WEnum<FailureReason>>)>> = const { Cell::new(ptr::null_mut()) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapturerState {
    Idle,
    Capturing,
    Paused,
}

struct FrameCapturer<'s> {
    stream: Pin<Box<dyn Future<Output = ()> + 's>>,
    buffer_slot: BufferSlot,
    state: CapturerState,
    restore_state: Option<CapturerState>,
    is_enabled: bool,
}

impl<'s> FrameCapturer<'s> {
    pub fn new(session: &'s Session) -> Self {
        let buffer_slot = BufferSlot::new();
        let buffer_slot_clone = buffer_slot.clone();
        let capture_loop = async move {
            fn send_tx(pw_buffer: PwBuffer, capture_res: Result<Frame, WEnum<FailureReason>>) {
                FRAME_CAPTURE_RESULT.with(|v| {
                    let ptr = v.get();
                    if !ptr.is_null() {
                        unsafe {
                            *ptr = Some((pw_buffer, capture_res));
                        }
                    } else {
                        log::error!("cannot send frame capture result because the pointer is null, the pointer shouldn't be null");
                    }
                })
            }
            loop {
                let pw_buffer = buffer_slot.clone().await;

                // XXX: Using full damage may be simpler than tracking partial damage regions.
                // Using partial damage could reduce copy costs, but it would add overhead
                // for tracking damage per buffer. Is the trade-off worth it?
                let full_damage = &[Rect {
                    x: 0,
                    y: 0,
                    width: pw_buffer.frame_size.0 as i32,
                    height: pw_buffer.frame_size.1 as i32,
                }];
                let wl_buffer = &pw_buffer.user_data.wl_buffer;
                let capture_res = session.capture_wl_buffer(wl_buffer, full_damage).await;
                send_tx(pw_buffer, capture_res);
            }
        };
        Self {
            stream: Box::pin(capture_loop),
            buffer_slot: buffer_slot_clone,
            state: CapturerState::Idle,
            restore_state: None,
            is_enabled: true,
        }
    }

    pub fn set_active(&mut self, active: bool) {
        if self.is_enabled == active {
            return;
        }

        self.is_enabled = active;
        match active {
            true => self.state = self.restore_state.take().unwrap_or(CapturerState::Idle),
            false => {
                self.restore_state = Some(self.state);
                self.state = CapturerState::Paused;
            }
        }
    }

    #[inline]
    pub fn state(&self) -> CapturerState {
        self.state
    }

    /// Capture to a given PipeWire buffer
    ///
    /// # Safety
    ///
    /// Ensure that the capturer state is `Idle` before calling this method.
    /// Otherwise, PipeWire buffers may be leaked.
    pub unsafe fn capture(&mut self, pw_buffer: PwBuffer) {
        self.buffer_slot.set(pw_buffer);
        self.state = CapturerState::Capturing;
    }
}

impl<'s> Future for FrameCapturer<'s> {
    type Output = (PwBuffer, Result<Frame, WEnum<FailureReason>>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if self.state() == CapturerState::Capturing {
            let mut capture_res: Option<(PwBuffer, Result<Frame, WEnum<FailureReason>>)> = None;
            FRAME_CAPTURE_RESULT.set(&mut capture_res);
            let _poll_res = self.stream.as_mut().poll(cx);
            FRAME_CAPTURE_RESULT.set(ptr::null_mut());

            match capture_res {
                Some(res) => {
                    self.state = CapturerState::Idle;
                    Poll::Ready(res)
                }
                None => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}

struct FrameTimer {
    timer: Pin<Box<time::Sleep>>,
    is_set: bool,
}

impl FrameTimer {
    pub fn is_set(&self) -> bool {
        self.is_set
    }

    pub fn set(&mut self, duration: Duration) {
        if self.is_set() {
            return;
        }

        self.timer
            .as_mut()
            .reset(tokio::time::Instant::now() + duration);
        self.is_set = true;
    }

    pub fn clear(&mut self) {
        self.is_set = false;
    }
}

impl Default for FrameTimer {
    fn default() -> Self {
        Self {
            timer: Box::pin(time::sleep(Duration::ZERO)),
            is_set: false,
        }
    }
}

impl Future for FrameTimer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if self.is_set {
            match self.timer.as_mut().poll(cx) {
                Poll::Ready(res) => {
                    self.is_set = false;
                    Poll::Ready(res)
                }
                Poll::Pending => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    }
}

struct FramePacing {
    last_frame: Option<Instant>,
    retry_count: u32,
    min_interval: Duration,
}

impl FramePacing {
    #[inline]
    pub fn time_until_next_frame(&self) -> Option<Duration> {
        if self.min_interval == Duration::ZERO {
            return None;
        }

        if let Some(last_frame_time) = self.last_frame {
            let time_since_last_frame = last_frame_time.elapsed();
            if time_since_last_frame < self.min_interval {
                let timeout = self.min_interval - time_since_last_frame;
                return Some(timeout);
            }
        }

        None
    }

    #[inline]
    pub fn retry_delay(&mut self) -> Duration {
        self.retry_count += 1;
        let retry_interval = Duration::from_millis(3) * self.retry_count;
        retry_interval.min(self.min_interval)
    }
}

impl Default for FramePacing {
    fn default() -> Self {
        Self {
            last_frame: None,
            retry_count: 0,
            min_interval: Duration::ZERO,
        }
    }
}

struct PwBackend<'s> {
    stream: pipewire::stream::StreamRc,
    _stream_listener: pipewire::stream::StreamListener<Rc<RefCell<StreamData<'s>>>>,
    _context: pipewire::context::ContextRc,
    mainloop: pipewire::main_loop::MainLoopRc,
    stream_data: Rc<RefCell<StreamData<'s>>>,
}

impl<'s> PwBackend<'s> {
    pub async fn new(
        wayland_helper: WaylandHelper,
        session: &'s Session,
        event_queue: &'s RefCell<VecDeque<StreamEvent>>,
    ) -> anyhow::Result<Self> {
        let mainloop = pipewire::main_loop::MainLoopRc::new(None)?;
        let context = pipewire::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None)?;

        let name = "cosmic-screenshot".to_string(); // XXX randomize?

        let stream = pipewire::stream::StreamRc::new(
            core,
            &name,
            pipewire::properties::properties! {
                "media.class" => "Video/Source",
                "node.name" => "cosmic-screenshot", // XXX
            },
        )?;

        let Some(formats) = session.wait_for_formats(|formats| formats.clone()).await else {
            return Err(anyhow::anyhow!(
                "failed to get formats for image copy; session stopped"
            ));
        };
        let dmabuf_helper = wayland_helper.dmabuf();
        let initial_params = format_params(dmabuf_helper.as_ref(), None, &formats);
        let mut initial_params: Vec<_> = initial_params.iter().map(|x| &**x).collect();

        //let flags = pipewire::stream::StreamFlags::MAP_BUFFERS;
        let flags =
            pipewire::stream::StreamFlags::ALLOC_BUFFERS | pipewire::stream::StreamFlags::DRIVER;
        stream.connect(
            spa::utils::Direction::Output,
            None,
            flags,
            &mut initial_params,
        )?;

        // Hacky: Use a temporary listener to wait for the node ID to be assigned.
        // Nothing important happens before the node ID is assigned, so we can safely ignore it.
        let is_ready = Cell::new(None);
        let mut _listener = stream
            .add_local_listener_with_user_data(&is_ready)
            .state_changed(|_stream, is_ready, old, new| {
                log::info!("state-changed '{:?}' -> '{:?}'", old, new);
                match new {
                    StreamState::Paused => {
                        is_ready.set(Some(Ok(())));
                    }
                    StreamState::Error(msg) => {
                        is_ready.set(Some(Err(msg)));
                    }
                    _ => {}
                }
            })
            .register()?;

        let loop_ = mainloop.loop_();
        // Time out after 60 seconds
        let mut is_node_id_assigned = false;
        for _ in 0..12 {
            loop_.iterate(Duration::from_secs(5));
            match is_ready.take() {
                Some(res) => match res {
                    Ok(_) => {
                        log::info!("Created a new screencast with node ID {}", stream.node_id());
                        is_node_id_assigned = true;
                        // Node ID assigned successfully
                        break;
                    }
                    Err(msg) => {
                        return Err(anyhow::Error::msg(msg));
                    }
                },
                None => {}
            }
        }
        _listener.unregister();
        if !is_node_id_assigned {
            return Err(anyhow::Error::msg("Cannot get PipeWire stream node ID"));
        }

        let stream_data = Rc::new(RefCell::new(StreamData {
            wayland_helper,
            dmabuf_helper,
            session,
            formats,
            format: gbm::Format::Abgr8888,
            modifier: None,
            event_queue,
        }));

        let stream_listener = stream
            .add_local_listener_with_user_data(stream_data.clone())
            .state_changed(|stream, data, old, new| {
                data.borrow_mut().state_changed(stream, old, new)
            })
            .param_changed(|stream, data, id, pod| data.borrow_mut().param_changed(stream, id, pod))
            .add_buffer(|stream, data, buffer| data.borrow_mut().add_buffer(stream, buffer))
            .remove_buffer(|stream, data, buffer| data.borrow_mut().remove_buffer(stream, buffer))
            .register()?;

        Ok(Self {
            stream,
            _stream_listener: stream_listener,
            _context: context,
            mainloop,
            stream_data,
        })
    }

    pub fn node_id(&self) -> u32 {
        self.stream.node_id()
    }

    pub unsafe fn dequeue_raw_buffer(&self) -> Option<NonNull<pw_buffer>> {
        NonNull::new(unsafe { self.stream.dequeue_raw_buffer() })
    }
}

struct ScreencastLoop<'s> {
    pw_backend: PwBackend<'s>,
    next_frame_timer: FrameTimer,
    frame_capturer: FrameCapturer<'s>,
    frame_pacing: FramePacing,
    event_queue: &'s RefCell<VecDeque<StreamEvent>>,
}

impl<'s> ScreencastLoop<'s> {
    pub fn new(
        pw_backend: PwBackend<'s>,
        session: &'s Session,
        event_queue: &'s RefCell<VecDeque<StreamEvent>>,
    ) -> Self {
        Self {
            pw_backend,
            next_frame_timer: FrameTimer::default(),
            frame_capturer: FrameCapturer::new(session),
            frame_pacing: FramePacing::default(),
            event_queue,
        }
    }

    /// Run the screencast loop until the stop receiver is received.
    pub async fn run_until(mut self, mut stop_rx: oneshot::Receiver<()>) {
        let pw_loop = self.pw_backend.mainloop.loop_();
        let Ok(pw_loop_fd) = tokio::io::unix::AsyncFd::new(pw_loop.fd()) else {
            log::error!("failed to create AsyncFd for PipeWire loop");
            return;
        };

        #[inline]
        async fn capture_handler(
            capture_res: (PwBuffer, Result<Frame, WEnum<FailureReason>>),
            frame_pacing: &mut FramePacing,
            next_frame_timer: &mut FrameTimer,
            frame_capturer: &mut FrameCapturer<'_>,
            pw_backend: &PwBackend<'_>,
        ) {
            let (pw_buffer, result) = capture_res;

            frame_pacing.last_frame = Some(
                Instant::now()
                    - Duration::from_nanos_u128(
                        pw_buffer
                            .timestamp
                            .elapsed()
                            .min(frame_pacing.min_interval)
                            .as_nanos()
                            >> 1,
                    ),
            );
            frame_pacing.retry_count = 0;

            if !pw_buffer.user_data.is_attached_with_pw_buffer.get() {
                // Buffer is invalid, so don't queue it to PipeWire stream.
                schedule_next_capture(frame_pacing, next_frame_timer, frame_capturer, pw_backend);
                return;
            }

            match result {
                Ok(frame) => {
                    if let Some(video_transform) = unsafe {
                        buffer_find_meta_data::<spa_sys::spa_meta_videotransform>(
                            pw_buffer.raw_buffer.as_ptr(),
                            spa_sys::SPA_META_VideoTransform,
                        )
                    } {
                        video_transform.transform = convert_transform(frame.transform);
                    }

                    if let Some(meta_damage) = unsafe {
                        buffer_find_meta(
                            pw_buffer.raw_buffer.as_ptr(),
                            spa_sys::SPA_META_VideoDamage,
                        )
                    } {
                        fn update_meta_region(
                            meta_region: &mut spa_sys::spa_meta_region,
                            x: i32,
                            y: i32,
                            width: u32,
                            height: u32,
                        ) {
                            meta_region.region.position.x = x;
                            meta_region.region.position.y = y;
                            meta_region.region.size.width = width;
                            meta_region.region.size.height = height;
                        }

                        let meta_region_len =
                            meta_damage.size as usize / size_of::<spa_sys::spa_meta_region>();
                        assert_eq!(
                            meta_region_len * size_of::<spa_sys::spa_meta_region>(),
                            meta_damage.size as usize
                        );
                        let frame_damage_len = frame.damage.len();

                        // SAFETY:
                        // The Video Damage metadata is initialized by PipeWire as a contiguous array
                        // of `spa_meta_region`. The type is `#[repr(C)]` POD (C integers only),
                        // and `meta_region_len` is bounded by the metadata size, so it is safe
                        // to treat as `&mut [spa_meta_region]`.
                        let meta_regions = unsafe {
                            let ptr = meta_damage.data as *mut spa_sys::spa_meta_region;
                            core::slice::from_raw_parts_mut(ptr, meta_region_len)
                        };
                        let mut meta_regions_iter = meta_regions.iter_mut();

                        if meta_region_len < frame_damage_len {
                            log::info!(
                                "Not enough buffers ({}) to accommodate damaged regions ({})",
                                meta_region_len,
                                frame_damage_len
                            );
                            // TODO: Merge damage properly
                            // SAFETY: We know that `meta_regions` is not empty because at least one buffer is allocated.
                            let meta_region = meta_regions_iter.next().unwrap();
                            update_meta_region(
                                meta_region,
                                0,
                                0,
                                pw_buffer.frame_size.0,
                                pw_buffer.frame_size.1,
                            );
                        } else {
                            frame.damage.iter().for_each(|rect| {
                                // SAFETY: We know that `meta_regions` length is equal or less than `frame_damage` length.
                                let meta_region = meta_regions_iter.next().unwrap();
                                update_meta_region(
                                    meta_region,
                                    rect.x,
                                    rect.y,
                                    rect.width as _,
                                    rect.height as _,
                                );
                            });
                        }

                        // Set invalid region to mark end of array
                        if let Some(meta_region) = meta_regions_iter.next() {
                            update_meta_region(meta_region, 0, 0, 0, 0);
                        }
                    }
                }
                Err(err) => {
                    if err == WEnum::Value(FailureReason::BufferConstraints) {
                        let changed = pw_backend
                            .stream_data
                            .borrow_mut()
                            .update_formats(pw_backend.stream.as_ref())
                            .await;

                        match changed {
                            true => {
                                // Buffer is invalid, so don't queue it to PipeWire stream.
                                pw_backend.mainloop.loop_().iterate(Duration::ZERO);
                                // TODO: Improve performance
                                // Performance degradation occurs during window resizing (when capturing a window).
                                // Frequent resize events cause PipeWire to continuously renegotiate,
                                // removing and re-allocating buffers rapidly.
                                return;
                            }
                            false => log::error!(
                                "screencopy buffer constraints error, but no new formats?"
                            ),
                        }
                    } else {
                        log::error!("screencopy failed: {:?}", err);
                        // TODO terminate screencasting?
                    }
                }
            }

            unsafe {
                pw_backend
                    .stream
                    .queue_raw_buffer(pw_buffer.raw_buffer.as_ptr())
            }
            schedule_next_capture(frame_pacing, next_frame_timer, frame_capturer, pw_backend);
        }

        #[inline]
        fn schedule_next_capture(
            frame_pacing: &mut FramePacing,
            next_frame_timer: &mut FrameTimer,
            frame_capturer: &mut FrameCapturer,
            pw_backend: &PwBackend,
        ) {
            let timeout = frame_pacing.time_until_next_frame();
            match timeout {
                Some(duration) => next_frame_timer.set(duration),
                None => try_capture_now(frame_pacing, next_frame_timer, frame_capturer, pw_backend),
            }
        }

        #[inline]
        fn try_capture_now(
            frame_pacing: &mut FramePacing,
            frame_timer: &mut FrameTimer,
            frame_capturer: &mut FrameCapturer,
            pw_backend: &PwBackend,
        ) {
            // If Frame Capturer state is not Idle, do nothing.
            if frame_capturer.state() != CapturerState::Idle {
                return;
            }

            match unsafe { pw_backend.dequeue_raw_buffer() } {
                Some(raw_buffer) => {
                    // SAFETY: The capturer state is Idle, so it is safe to call capture.
                    unsafe {
                        frame_capturer.capture(PwBuffer::from_raw_buffer(
                            raw_buffer,
                            pw_backend.stream_data.borrow().formats.buffer_size,
                        ));
                    }
                }
                // TODO
                // If PipeWire runs out of buffers, schedule a retry after a short delay.
                // Otherwise, the capture loop effectively stalls, as the next frame scheduling
                // depends on the completion of the current one.

                // It would be better and safer if we had a way to know exactly when the capture
                // source has rendered a frame, and invoke this function immediately afterward.
                None => frame_timer.set(frame_pacing.retry_delay()),
            }
        }

        loop {
            while let Some(event) = self.event_queue.borrow_mut().pop_front() {
                match event {
                    StreamEvent::MinFrameInterval(min_interval_us) => {
                        self.frame_pacing.min_interval = Duration::from_micros(min_interval_us);
                    }
                    StreamEvent::Streaming => {
                        self.frame_capturer.set_active(true);
                        schedule_next_capture(
                            &mut self.frame_pacing,
                            &mut self.next_frame_timer,
                            &mut self.frame_capturer,
                            &self.pw_backend,
                        )
                    }
                    StreamEvent::Paused => {
                        self.frame_capturer.set_active(false);
                        self.next_frame_timer.clear();
                    }
                    StreamEvent::Error => {
                        log::info!("Exit the screencast by PipeWire stream error");
                        break;
                    }
                }
            }

            tokio::select! {
                biased;

                Ok(mut guard) = pw_loop_fd.readable() => {
                    pw_loop.iterate(Duration::ZERO);
                    guard.clear_ready();
                }

                capture_res = &mut self.frame_capturer => {
                    capture_handler(capture_res, &mut self.frame_pacing, &mut self.next_frame_timer, &mut self.frame_capturer, &self.pw_backend).await
                }

                _ = &mut self.next_frame_timer => {
                    try_capture_now(&mut self.frame_pacing, &mut self.next_frame_timer, &mut self.frame_capturer, &self.pw_backend)
                }

                _ = &mut stop_rx => {
                    log::info!("Exit the screencast by stopping the signal");
                    break;
                }
            }
        }

        // Clean up resources before exiting
        self.next_frame_timer.clear();
        pw_loop.iterate(Duration::ZERO);
    }
}

pub struct ScreencastThread {
    stream_props: StreamProps,
    node_id: u32,
    thread_stop_tx: oneshot::Sender<()>,
}

impl ScreencastThread {
    pub async fn new(
        wayland_helper: WaylandHelper,
        capture_source: CaptureSource,
        overlay_cursor: bool,
        stream_props: StreamProps,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = oneshot::channel();
        let (thread_stop_tx, thread_stop_rx) = oneshot::channel::<()>();
        std::thread::spawn(move || {
            /// Sends a message back to the main thread or exits the current block.
            macro_rules! tx_send_or_exit {
                ($mess:expr, $context:expr) => {
                    // If tx send fails, that means the main thread has already exited.
                    // So we exit this thread too.
                    if let Err(_) = tx.send($mess) {
                        log::error!("failed to send message back. Context: {}", $context);
                        return;
                    }
                };
            }

            /// Returns the result of the expression or exits the current block.
            macro_rules! unwrap_or_exit {
                ($result:expr, $context:expr) => {
                    match $result {
                        Ok(v) => v,
                        Err(e) => {
                            let err = anyhow::Error::from(e).context($context);
                            tx_send_or_exit!(Err(err), $context);
                            return;
                        }
                    }
                };
            }

            let rt = unwrap_or_exit!(
                tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build(),
                "failed to build tokio runtime"
            );
            rt.block_on(async move {
                let session = wayland_helper.capture_source_session(capture_source, overlay_cursor);
                let event_queue = RefCell::new(VecDeque::with_capacity(5));
                let pw_backend = unwrap_or_exit!(
                    PwBackend::new(wayland_helper, &session, &event_queue).await,
                    "failed to create Pipewire backend"
                );
                let node_id = pw_backend.node_id();
                tx_send_or_exit!(Ok(node_id), "failed to send node ID");
                let screencast_loop = ScreencastLoop::new(pw_backend, &session, &event_queue);
                screencast_loop.run_until(thread_stop_rx).await;
            });
        });

        Ok(Self {
            stream_props,
            node_id: rx.await.unwrap()?,
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

struct StreamData<'s> {
    dmabuf_helper: Option<DmabufHelper>,
    wayland_helper: WaylandHelper,
    format: gbm::Format,
    modifier: Option<gbm::Modifier>,
    session: &'s Session,
    formats: Formats,
    event_queue: &'s RefCell<VecDeque<StreamEvent>>,
}

impl<'s> StreamData<'s> {
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
    async fn update_formats(&mut self, stream: &Stream) -> bool {
        let Some(formats) = self
            .session
            .wait_for_formats(|formats| formats.clone())
            .await
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

    fn state_changed(&mut self, _stream: &Stream, old: StreamState, new: StreamState) {
        log::info!("state-changed '{:?}' -> '{:?}'", old, new);
        match new {
            StreamState::Streaming => self
                .event_queue
                .borrow_mut()
                .push_back(StreamEvent::Streaming),
            StreamState::Paused => self.event_queue.borrow_mut().push_back(StreamEvent::Paused),
            StreamState::Error(_) => self.event_queue.borrow_mut().push_back(StreamEvent::Error),
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

        if pwr_format.max_framerate().num > 0 {
            let min_interval_us = USEC_PER_SEC * pwr_format.max_framerate().denom as u64
                / pwr_format.max_framerate().num as u64;
            self.event_queue
                .borrow_mut()
                .push_front(StreamEvent::MinFrameInterval(min_interval_us));
        } else {
            self.event_queue
                .borrow_mut()
                .push_front(StreamEvent::MinFrameInterval(0));
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

        let user_data = Rc::into_raw(Rc::new(PwBufferUserData {
            wl_buffer,
            is_attached_with_pw_buffer: Cell::new(true),
        })) as *mut c_void;
        unsafe { (*buffer).user_data = user_data };
    }

    fn remove_buffer(&mut self, _stream: &Stream, buffer: *mut pw_buffer) {
        let buf = unsafe { &mut *(*buffer).buffer };
        let datas = unsafe { slice::from_raw_parts_mut(buf.datas, buf.n_datas as usize) };

        for data in datas {
            unsafe { rustix::io::close(data.fd as _) };
            data.fd = -1;
        }

        let user_data: Rc<PwBufferUserData> =
            unsafe { Rc::from_raw((*buffer).user_data as *mut _) };
        user_data.is_attached_with_pw_buffer.set(false);
    }
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
    unsafe {
        let ptr = spa_sys::spa_buffer_find_meta_data((*buffer).buffer, type_, size_of::<T>());
        (ptr as *mut T).as_mut()
    }
}

// SAFETY: buffer must be non-null, and valid as long as return value is used
unsafe fn buffer_find_meta<'a>(
    buffer: *const pipewire_sys::pw_buffer,
    type_: u32,
) -> Option<&'a mut spa_sys::spa_meta> {
    unsafe {
        let ptr = spa_sys::spa_buffer_find_meta((*buffer).buffer, type_);
        (ptr as *mut spa_sys::spa_meta).as_mut()
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

fn transform_meta() -> OwnedPod {
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
}

fn damage_meta() -> OwnedPod {
    OwnedPod::serialize(&pod::Value::Object(pod::Object {
        type_: spa_sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa_sys::SPA_PARAM_Meta,
        properties: vec![
            pod::Property {
                key: spa_sys::SPA_PARAM_META_type,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Id(spa::utils::Id(spa_sys::SPA_META_VideoDamage)),
            },
            pod::Property {
                key: spa_sys::SPA_PARAM_META_size,
                flags: pod::PropertyFlags::empty(),
                value: pod::Value::Choice(pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Range {
                        default: (size_of::<spa_sys::spa_meta_region>() * 4) as _,
                        min: size_of::<spa_sys::spa_meta_region>() as _,
                        max: (size_of::<spa_sys::spa_meta_region>() * 32) as _,
                    },
                ))),
            },
        ],
    }))
}
// TODO: header

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
        Some(transform_meta()),
        Some(damage_meta()),
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
    let default_framerate = spa_sys::spa_fraction { num: 60, denom: 1 };
    let min_framerate = spa_sys::spa_fraction { num: 0, denom: 1 };
    // Is there any way to get the maximum refresh rate?
    let max_framerate = spa_sys::spa_fraction {
        num: 1000,
        denom: 1,
    };

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
            value: pod::Value::Fraction(spa::utils::Fraction { num: 0, denom: 1 }),
        },
        pod::Property {
            key: spa_sys::SPA_FORMAT_VIDEO_maxFramerate,
            flags: pod::PropertyFlags::empty(),
            value: pod::Value::Choice(pod::ChoiceValue::Fraction(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Range {
                    default: default_framerate,
                    min: min_framerate,
                    max: max_framerate,
                },
            ))),
        },
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
