use cosmic_client_toolkit::screencopy::{CaptureSession, FailureReason, Frame};
use futures::channel::oneshot;
use std::{
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};
use wayland_client::{QueueHandle, WEnum};

use super::{AppData, CursorCaptureSessionData, FrameData};

enum State {
    WaitingForFormats,
    Capturing(oneshot::Receiver<Result<Frame, WEnum<FailureReason>>>),
}

// TODO wake stream when we get formats?
pub struct CursorStream {
    // TODO formats
    capture_session: CaptureSession,
    qh: QueueHandle<AppData>,
}

impl CursorStream {
    pub(super) fn new(capture_session: &CaptureSession, qh: &QueueHandle<AppData>) -> Self {
        Self {
            capture_session: capture_session.clone(),
            qh: qh.clone(),
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
        let formats = data.formats.lock().unwrap().clone();
        // WIP damage
        self.capture_session.capture(
            todo!(),
            &[],
            &self.qh,
            FrameData {
                frame_data: Default::default(),
                sender: Mutex::new(Some(sender)),
            },
        );
        todo!()
    }
}
