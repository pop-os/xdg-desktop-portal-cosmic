use cosmic_client_toolkit::screencopy::{CaptureSession, FailureReason, Frame};
use futures::channel::oneshot;
use std::{
    future::Future,
    os::fd::{AsFd, OwnedFd},
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};
use wayland_client::{
    QueueHandle, WEnum,
    protocol::{wl_buffer, wl_shm},
};

use super::{AppData, CursorCaptureSessionData, FrameData, WaylandHelper};
use crate::buffer;

enum State {
    WaitingForFormats,
    Capturing(oneshot::Receiver<Result<Frame, WEnum<FailureReason>>>),
}

// TODO wake stream when we get formats?
pub struct CursorStream {
    state: Mutex<State>,
    // TODO formats
    capture_session: CaptureSession,
    wayland_helper: WaylandHelper,
    // XXX modify pin without mutex?
    buffer: Mutex<Option<(u32, u32, OwnedFd, wl_buffer::WlBuffer)>>,
}

impl CursorStream {
    pub(super) fn new(capture_session: &CaptureSession, wayland_helper: &WaylandHelper) -> Self {
        Self {
            state: Mutex::new(State::WaitingForFormats),
            capture_session: capture_session.clone(),
            wayland_helper: wayland_helper.clone(),
            buffer: Mutex::new(None),
        }
    }
}

impl futures::stream::Stream for CursorStream {
    type Item = image::RgbaImage;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<image::RgbaImage>> {
        let data = self
            .capture_session
            .data::<CursorCaptureSessionData>()
            .unwrap();
        *data.waker.lock().unwrap() = Some(cx.waker().clone());

        let mut buffer = self.buffer.lock().unwrap();
        let mut state = self.state.lock().unwrap();

        if let Some(formats) = &data.formats.lock().unwrap().clone() {
            // XXX test if res changed
            if buffer
                .as_ref()
                .is_none_or(|(w, h, _, _)| (*w, *h) != formats.buffer_size)
            {
                let (width, height) = formats.buffer_size;
                let fd = buffer::create_memfd(width, height);
                let wl_buffer = self.wayland_helper.create_shm_buffer(
                    &fd,
                    width,
                    height,
                    width * 4,
                    wl_shm::Format::Argb8888,
                );
                *buffer = Some((width, height, fd, wl_buffer));
                *state = State::WaitingForFormats; // XXX, well, not waiting
            }
        }

        if let State::Capturing(receiver) = &mut *state {
            match std::pin::Pin::new(receiver).poll(cx) {
                Poll::Ready(Ok(frame)) => {
                    // TODO map buffer
                    let (width, height, fd, _) = &buffer.as_ref().unwrap();
                    // XXX unwrap
                    let mmap = unsafe { memmap2::Mmap::map(fd).unwrap() };
                    let mut bytes = mmap.to_vec();
                    // Swap BGRA to RGBA
                    for pixel in bytes.chunks_mut(4) {
                        pixel.swap(2, 0);
                    }
                    let image = image::RgbaImage::from_vec(*width, *height, bytes);
                    return Poll::Ready(image);
                }
                // XXX Ignore error
                Poll::Ready(Err(_err)) => {}
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }

        if let Some((_, _, _, wl_buffer)) = &*buffer {
            let (sender, receiver) = oneshot::channel();
            // WIP damage
            self.capture_session.capture(
                wl_buffer,
                &[],
                &self.wayland_helper.inner.qh,
                FrameData {
                    frame_data: Default::default(),
                    sender: Mutex::new(Some(sender)),
                },
            );
            *state = State::Capturing(receiver);
        }

        Poll::Pending
    }
}
