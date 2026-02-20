use cosmic_client_toolkit::screencopy::{CaptureSession, FailureReason, Frame, Rect};
use futures::channel::oneshot;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use wayland_client::WEnum;
use wayland_client::protocol::{wl_buffer, wl_shm};

use super::{CursorCaptureSessionData, FrameData, WaylandHelper};
use crate::buffer;

enum State {
    WaitingForFormats,
    Capturing(oneshot::Receiver<Result<Frame, WEnum<FailureReason>>>),
}

struct CursorStreamBuffer {
    fd: OwnedFd,
    wl_buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    is_new: bool,
}

// TODO wake stream when we get formats?
pub struct CursorStream {
    state: Mutex<State>,
    // TODO formats
    capture_session: CaptureSession,
    wayland_helper: WaylandHelper,
    // XXX modify pin without mutex?
    buffer: Mutex<Option<CursorStreamBuffer>>,
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
                .is_none_or(|b| (b.width, b.height) != formats.buffer_size)
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
                *buffer = Some(CursorStreamBuffer {
                    width,
                    height,
                    fd,
                    wl_buffer,
                    is_new: true,
                });
                *state = State::WaitingForFormats; // XXX, well, not waiting
            }
        }

        if let State::Capturing(receiver) = &mut *state {
            match std::pin::Pin::new(receiver).poll(cx) {
                Poll::Ready(Ok(frame)) => {
                    // TODO map buffer
                    let CursorStreamBuffer {
                        width,
                        height,
                        fd,
                        is_new,
                        ..
                    } = buffer.as_mut().unwrap();
                    *is_new = false;
                    // XXX unwrap
                    let mmap = unsafe { memmap2::Mmap::map(&*fd).unwrap() };
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

        if let Some(CursorStreamBuffer {
            width,
            height,
            wl_buffer,
            is_new,
            ..
        }) = &*buffer
        {
            let (sender, receiver) = oneshot::channel();
            let full_damage = Rect {
                x: 0,
                y: 0,
                width: *width as i32,
                height: *height as i32,
            };
            let damage = if *is_new {
                std::slice::from_ref(&full_damage)
            } else {
                &[]
            };
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
