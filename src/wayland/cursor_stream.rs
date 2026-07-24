use cosmic_client_toolkit::screencopy::{CaptureSession, FailureReason, Frame, Rect};
use futures::channel::oneshot;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use wayland_client::WEnum;
use wayland_client::protocol::{wl_buffer, wl_shm};

use super::{
    CursorCaptureSessionData, CursorSession, CursorSessionInner, FrameData, WaylandHelper,
    wl_transform_to_image_orientation,
};
use crate::buffer;

enum State {
    Waiting,
    Capturing(oneshot::Receiver<Result<Frame, WEnum<FailureReason>>>),
}

struct CursorStreamBuffer {
    fd: OwnedFd,
    wl_buffer: wl_buffer::WlBuffer,
    width: u32,
    height: u32,
    is_new: bool,
}

pub struct CursorFrame {
    pub image: image::RgbaImage,
    pub hotspot: (i32, i32),
}

// TODO wake stream when we get formats?
pub struct CursorStream {
    state: State,
    cursor_session: Weak<CursorSessionInner>,
    capture_session: CaptureSession,
    wayland_helper: WaylandHelper,
    buffer: Option<CursorStreamBuffer>,
}

impl CursorStream {
    pub(super) fn new(
        cursor_session: &CursorSession,
        capture_session: &CaptureSession,
        wayland_helper: &WaylandHelper,
    ) -> Self {
        Self {
            state: State::Waiting,
            cursor_session: Arc::downgrade(&cursor_session.0),
            capture_session: capture_session.clone(),
            wayland_helper: wayland_helper.clone(),
            buffer: None,
        }
    }
}

impl futures::stream::Stream for CursorStream {
    type Item = CursorFrame;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<CursorFrame>> {
        let stream = self.get_mut();

        let data = stream
            .capture_session
            .data::<CursorCaptureSessionData>()
            .unwrap();
        *data.waker.lock().unwrap() = Some(cx.waker().clone());

        // Allocate buffer if we don't have one, or resolution has changed
        if let Some(formats) = &data.formats.lock().unwrap().clone()
            && stream
                .buffer
                .as_ref()
                .is_none_or(|b| (b.width, b.height) != formats.buffer_size)
        {
            let (width, height) = formats.buffer_size;
            let fd = buffer::create_memfd(width, height);
            let wl_buffer = stream.wayland_helper.create_shm_buffer(
                &fd,
                width,
                height,
                width * 4,
                wl_shm::Format::Argb8888,
            );
            stream.buffer = Some(CursorStreamBuffer {
                width,
                height,
                fd,
                wl_buffer,
                is_new: true,
            });
            stream.state = State::Waiting;
        }

        // If we are capturing, poll receiver for result
        if let State::Capturing(receiver) = &mut stream.state {
            match std::pin::Pin::new(receiver).poll(cx) {
                Poll::Ready(Ok(Ok(frame))) => {
                    let Some(cursor_session) = stream.cursor_session.upgrade().map(CursorSession)
                    else {
                        return Poll::Ready(None);
                    };

                    let CursorStreamBuffer {
                        width,
                        height,
                        fd,
                        is_new,
                        ..
                    } = stream.buffer.as_mut().unwrap();
                    *is_new = false;
                    let mmap = unsafe { memmap2::Mmap::map(&*fd).unwrap() };
                    let mut bytes = mmap.to_vec();
                    // Swap BGRA to RGBA
                    for pixel in bytes.chunks_mut(4) {
                        pixel.swap(2, 0);
                    }
                    let Some(image) = image::RgbaImage::from_vec(*width, *height, bytes) else {
                        return Poll::Ready(None);
                    };
                    let transform = match frame.transform {
                        WEnum::Value(value) => value,
                        WEnum::Unknown(value) => panic!("invalid capture transform: {}", value),
                    };
                    let mut image = image::DynamicImage::from(image);
                    image.apply_orientation(wl_transform_to_image_orientation(transform));
                    let image = match image {
                        image::DynamicImage::ImageRgba8(image) => image,
                        _ => unreachable!(),
                    };
                    return Poll::Ready(Some(CursorFrame {
                        image,
                        hotspot: cursor_session.cursor_hotspot(),
                    }));
                }
                Poll::Ready(Ok(Err(reason))) => match reason {
                    WEnum::Value(FailureReason::BufferConstraints) => {
                        stream.buffer = None;
                    }
                    WEnum::Value(FailureReason::Stopped) => {
                        return Poll::Ready(None);
                    }
                    _ => tracing::error!("error in cursor capture: {:?})", reason),
                },
                Poll::Ready(Err(futures::channel::oneshot::Canceled)) => {}
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
        }) = &stream.buffer
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
            stream.capture_session.capture(
                wl_buffer,
                damage,
                &stream.wayland_helper.inner.qh,
                FrameData {
                    frame_data: Default::default(),
                    sender: Mutex::new(Some(sender)),
                },
            );
            stream.state = State::Capturing(receiver);
        }

        Poll::Pending
    }
}
