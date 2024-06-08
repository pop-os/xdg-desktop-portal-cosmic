// TODO testing for vaapi, nvenc
// Test modifiers, when added to pipewire gstreamersrc:
// - https://gitlab.freedesktop.org/pipewire/pipewire/-/merge_requests/1881

use ashpd::desktop::screencast::{CursorMode, PersistMode, Screencast, SourceType};
use gst::prelude::*;

use std::os::fd::AsRawFd;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    gst::init().unwrap();

    let screencast = Screencast::new().await?;
    let session = screencast.create_session().await?;
    screencast
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            true,
            None,
            PersistMode::DoNot,
        )
        .await?;
    let streams = screencast
        .start(&session, &ashpd::WindowIdentifier::default())
        .await?
        .response()?;
    println!("{} streams", streams.streams().len());
    let stream = &streams.streams()[0];
    let fd = screencast.open_pipe_wire_remote(&session).await?;

    let node_id = stream.pipe_wire_node_id();

    let src = gst::parse_bin_from_description(
        &format!(
            "pipewiresrc fd={} path={node_id} !
             capsfilter caps=video/x-raw(memory:DMABuf),format=RGBA",
             fd.as_raw_fd()
        ),
        true,
    )?;

    let sink = gst::parse_bin_from_description("waylandsink", true)?;
    // let sink = gst::parse_bin_from_description("glupload ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 ! gldownload ! video/x-raw(memory:DMABuf) ! vaapih264enc ! h264parse ! mp4mux ! filesink location=out.mp4", true)?;
    // let sink = gst::parse_bin_from_description("glupload ! video/x-raw(memory:GLMemory) ! nvh264enc ! h264parse ! mp4mux ! filesink location=out.mp4", true)?;

    let pipeline = gst::Pipeline::default()
        .dynamic_cast::<gst::Pipeline>()
        .unwrap();
    pipeline.add_many([&src, &sink])?;
    gst::Element::link_many([&src, &sink])?;

    pipeline.set_state(gst::State::Playing)?;
    let bus = pipeline.bus().unwrap();
    for _msg in bus.iter_timed(gst::ClockTime::NONE) {}

    Ok(())
}
