use cosmic_client_toolkit::screencopy::{CaptureSession, FailureReason, Frame};
use futures::channel::oneshot;
use std::{
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};
use wayland_client::{
    protocol::{wl_buffer, wl_shm},
    QueueHandle, WEnum,
};

use super::{AppData, CursorCaptureSessionData, FrameData, WaylandHelper};
use crate::buffer;

enum State {
    WaitingForFormats,
    Capturing(oneshot::Receiver<Result<Frame, WEnum<FailureReason>>>),
}

// TODO wake stream when we get formats?
pub struct CursorStream {
    // TODO formats
    capture_session: CaptureSession,
    wayland_helper: WaylandHelper,
    // XXX modify pin without mutex?
    buffer: Mutex<Option<(u32, u32, wl_buffer::WlBuffer)>>,
}

impl CursorStream {
    pub(super) fn new(capture_session: &CaptureSession, wayland_helper: &WaylandHelper) -> Self {
        Self {
            capture_session: capture_session.clone(),
            wayland_helper: wayland_helper.clone(),
            buffer: Mutex::new(None),
        }
    }
}

impl futures::stream::Stream for CursorStream {
    type Item = image::RgbaImage;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<image::RgbaImage>> {
        let (sender, receiver) = oneshot::channel();
        let data = self
            .capture_session
            .data::<CursorCaptureSessionData>()
            .unwrap();
        *data.waker.lock().unwrap() = Some(cx.waker().clone());

        if let Some(formats) = &data.formats.lock().unwrap().clone() {
            // XXX test if res changed
            let mut buffer = self.buffer.lock().unwrap();
            if buffer.is_none() {
                let (width, height) = formats.buffer_size;
                let fd = buffer::create_memfd(width, height);
                let wl_buffer = self.wayland_helper.create_shm_buffer(
                    &fd,
                    width,
                    height,
                    width * 4,
                    wl_shm::Format::Abgr8888,
                );
                *buffer = Some((width, height, wl_buffer));
            }
        }

        // WIP damage
        self.capture_session.capture(
            todo!(),
            &[],
            &self.wayland_helper.inner.qh,
            FrameData {
                frame_data: Default::default(),
                sender: Mutex::new(Some(sender)),
            },
        );
        todo!()
    }
}
