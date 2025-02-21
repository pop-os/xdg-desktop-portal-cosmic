// TODO testing for vaapi, nvenc
// Test modifiers, when added to pipewire gstreamersrc:
// - https://gitlab.freedesktop.org/pipewire/pipewire/-/merge_requests/1881

use ashpd::desktop::PersistMode;
use ashpd::{
    desktop::screencast::{CursorMode, Screencast, SourceType},
    enumflags2::BitFlags,
};
use clap::Parser;
use gst::prelude::*;

use std::os::fd::AsRawFd;

#[derive(clap::Parser, Default, Debug, Clone, PartialEq, Eq)]
#[command(version, about, long_about = None)]
struct Args {
    /// Allow selecting multiple sources
    #[clap(long,
        default_missing_value("true"),
        default_value("true"),
        num_args(0..=1),
        require_equals(true),
        action = clap::ArgAction::Set)]
    multiple: bool,
    #[clap(long, value_enum, value_delimiter(','))]
    source_types: Vec<Source>,
}

#[derive(clap::ValueEnum, Debug, Copy, Clone, PartialEq, Eq)]
enum Source {
    Monitor,
    Window,
    Virtual,
}

impl From<Source> for SourceType {
    fn from(source: Source) -> SourceType {
        match source {
            Source::Monitor => SourceType::Monitor,
            Source::Window => SourceType::Window,
            Source::Virtual => SourceType::Virtual,
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    gst::init().unwrap();

    let args = Args::parse();

    let source_types = args
        .source_types
        .into_iter()
        .map(SourceType::from)
        .fold(BitFlags::EMPTY, |a, b| a | b);

    let screencast = Screencast::new().await?;
    let session = screencast.create_session().await?;
    screencast
        .select_sources(
            &session,
            CursorMode::Embedded,
            source_types,
            args.multiple,
            None,
            PersistMode::DoNot,
        )
        .await?;
    let streams = screencast.start(&session, None).await?.response()?;
    println!("{} streams", streams.streams().len());
    let stream = &streams.streams()[0];
    let fd = screencast.open_pipe_wire_remote(&session).await?;

    let node_id = stream.pipe_wire_node_id();

    // TODO Set drm-format; gstreamer Wayland plugins may still be missing some
    // needed support
    let format = if gst::version() > (1, 24, 0, 0) {
        "DMA_DRM"
    } else {
        "RGBA"
    };

    let src = gst::parse::bin_from_description(
        &format!(
            "pipewiresrc fd={} path={node_id} !
             capsfilter caps=video/x-raw(memory:DMABuf),format={}",
            fd.as_raw_fd(),
            format
        ),
        true,
    )?;

    let sink = gst::parse::bin_from_description("waylandsink", true)?;
    // let sink = gst::parse::bin_from_description("glupload ! glcolorconvert ! video/x-raw(memory:GLMemory),format=NV12 ! gldownload ! video/x-raw(memory:DMABuf) ! vaapih264enc ! h264parse ! mp4mux ! filesink location=out.mp4", true)?;
    // let sink = gst::parse::bin_from_description("glupload ! video/x-raw(memory:GLMemory) ! nvh264enc ! h264parse ! mp4mux ! filesink location=out.mp4", true)?;

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
