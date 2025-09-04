use cosmic_client_toolkit::screencopy::CaptureSession;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

// TODO wake stream when we get formats?
pub struct CursorStream {
    // TODO formats
    pub(super) capture_session: CaptureSession,
}

impl futures::stream::Stream for CursorStream {
    type Item = image::RgbaImage;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<image::RgbaImage>> {
        todo!()
    }
}
