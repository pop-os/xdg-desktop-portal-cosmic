// Live PickColor portal implementation.
//
// Shows a transparent fullscreen layer surface over every output. As the user
// moves the pointer, a small swatch next to the cursor is updated with the
// color currently under the pointer (sampled via cosmic-comp's
// ColorUnderCursor D-Bus method, which resolves the pointer position
// compositor-side so picking stays correct under screen magnification).
// A left-click returns the sampled color to the caller.
//
// Preview sampling is bounded: at most one pick is in flight at a time, and no
// more than one pick per MIN_INTERVAL is dispatched. A motion arriving while a
// pick is in flight sets `pending`, which is dispatched as soon as the previous
// pick completes.

use std::time::{Duration, Instant};

use cosmic::iced::core::Length;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::{Limits, window};
use cosmic::widget::space;
use cosmic_client_toolkit::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::PortalResponse;
use crate::app::{CosmicPortal, OutputState};
use crate::widget::color_picker::PickerArea;
use crate::widget::keyboard_wrapper::KeyboardWrapper;

/// Lower bound on the interval between ColorUnderCursor dispatches during preview.
const MIN_INTERVAL: Duration = Duration::from_millis(16);

#[zbus::proxy(
    interface = "com.system76.CosmicComp.ColorPicker",
    default_service = "com.system76.CosmicComp",
    default_path = "/com/system76/CosmicComp/ColorPicker"
)]
trait CosmicCompColorPicker {
    /// Sample the colour under the compositor's current pointer. No coordinates
    /// are sent: the compositor resolves the pointer position and any active
    /// screen-magnifier transform itself, which keeps picking correct under
    /// zoom and fractional scaling.
    fn color_under_cursor(&self) -> zbus::Result<(f64, f64, f64)>;
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct PickColorResult {
    color: (f64, f64, f64),
}

#[derive(Clone, Debug)]
pub struct Args {
    pub handle: zvariant::ObjectPath<'static>,
    pub app_id: String,
    pub parent_window: String,
    pub tx: Sender<PortalResponse<PickColorResult>>,
    /// The portal's main session-bus connection, reused
    /// so cosmic-comp's NameOwners check passes.
    pub connection: zbus::Connection,
    pub preview: Option<(f64, f64, f64)>,
    pub pick_in_flight: bool,
    pub last_dispatch: Option<Instant>,
    /// A newer pointer motion arrived that still needs a preview sample.
    pub pending: bool,
    pub finalizing: bool,
}

impl Args {
    pub fn new(
        handle: zvariant::ObjectPath<'static>,
        app_id: String,
        parent_window: String,
        tx: Sender<PortalResponse<PickColorResult>>,
        connection: zbus::Connection,
    ) -> Self {
        Self {
            handle,
            app_id,
            parent_window,
            tx,
            connection,
            preview: None,
            pick_in_flight: false,
            last_dispatch: None,
            pending: false,
            finalizing: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Motion,
    Clicked,
    Cancel,
    Previewed(Result<(f64, f64, f64), String>),
    Picked(Result<(f64, f64, f64), String>),
}

pub fn update_args(portal: &mut CosmicPortal, args: Args) -> cosmic::Task<crate::app::Msg> {
    if portal.color_picker_args.replace(args).is_some() {
        log::info!("Existing color picker args replaced");
        return cosmic::Task::none();
    }
    let cmds: Vec<_> = portal
        .outputs
        .iter()
        .map(|OutputState { output, id, .. }| {
            get_layer_surface(SctkLayerSurfaceSettings {
                id: *id,
                layer: Layer::Overlay,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                input_zone: None,
                anchor: Anchor::all(),
                output: IcedOutput::Output(output.clone()),
                namespace: "color_picker".to_string(),
                size: Some((None, None)),
                exclusive_zone: -1,
                size_limits: Limits::NONE.min_height(1.0).min_width(1.0),
                ..Default::default()
            })
        })
        .collect();
    cosmic::Task::batch(cmds)
}

pub fn view(portal: &CosmicPortal, id: window::Id) -> cosmic::Element<'_, Msg> {
    if !portal.outputs.iter().any(|o| o.id == id) {
        return space::horizontal().width(Length::Fixed(1.0)).into();
    }
    let Some(args) = portal.color_picker_args.as_ref() else {
        return space::horizontal().width(Length::Fixed(1.0)).into();
    };
    let preview = args.preview;
    KeyboardWrapper::new(
        PickerArea::new(move |_pos| Msg::Motion, move |_pos| Msg::Clicked, preview),
        |key, _mods| match key {
            cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Escape) => {
                Some(Msg::Cancel)
            }
            _ => None,
        },
    )
    .into()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    match msg {
        Msg::Motion => {
            let Some(args) = portal.color_picker_args.as_mut() else {
                return cosmic::Task::none();
            };
            if args.finalizing {
                return cosmic::Task::none();
            }
            args.pending = true;
            try_dispatch_preview(portal)
        }
        Msg::Previewed(res) => {
            let Some(args) = portal.color_picker_args.as_mut() else {
                return cosmic::Task::none();
            };
            args.pick_in_flight = false;
            if let Ok(rgb) = res {
                args.preview = Some(rgb);
            }
            try_dispatch_preview(portal)
        }
        Msg::Clicked => {
            let Some(args) = portal.color_picker_args.as_mut() else {
                return cosmic::Task::none();
            };
            args.finalizing = true;
            let conn = args.connection.clone();
            let mut cmds: Vec<_> = portal
                .outputs
                .iter()
                .map(|o| destroy_layer_surface(o.id))
                .collect();
            cmds.push(cosmic::Task::perform(
                async move { color_under_cursor_via_dbus(conn).await },
                |res| crate::app::Msg::ColorPicker(Msg::Picked(res)),
            ));
            cosmic::Task::batch(cmds)
        }
        Msg::Picked(res) => {
            let response = match res {
                Ok(rgb) => PortalResponse::Success(PickColorResult { color: rgb }),
                Err(e) => {
                    log::error!("Color pick failed: {e}");
                    PortalResponse::Other
                }
            };
            finish(portal, response, /* destroy_surfaces */ false)
        }
        Msg::Cancel => finish(portal, PortalResponse::Cancelled, true),
    }
}

fn try_dispatch_preview(portal: &mut CosmicPortal) -> cosmic::Task<crate::app::Msg> {
    let args = match portal.color_picker_args.as_mut() {
        Some(a) => a,
        None => return cosmic::Task::none(),
    };
    if args.pick_in_flight || args.finalizing {
        return cosmic::Task::none();
    }
    if let Some(last) = args.last_dispatch
        && last.elapsed() < MIN_INTERVAL
    {
        return cosmic::Task::none();
    }
    if !args.pending {
        return cosmic::Task::none();
    }
    args.pending = false;
    args.pick_in_flight = true;
    args.last_dispatch = Some(Instant::now());
    let conn = args.connection.clone();
    cosmic::Task::perform(
        async move { color_under_cursor_via_dbus(conn).await },
        |res| crate::app::Msg::ColorPicker(Msg::Previewed(res)),
    )
}

fn finish(
    portal: &mut CosmicPortal,
    response: PortalResponse<PickColorResult>,
    destroy_surfaces: bool,
) -> cosmic::Task<crate::app::Msg> {
    let cmds: Vec<_> = if destroy_surfaces {
        portal
            .outputs
            .iter()
            .map(|o| destroy_layer_surface(o.id))
            .collect()
    } else {
        Vec::new()
    };
    if let Some(args) = portal.color_picker_args.take() {
        let tx = args.tx;
        tokio::spawn(async move {
            if let Err(err) = tx.send(response).await {
                log::error!("Failed to send color picker response: {err}");
            }
        });
    }
    cosmic::Task::batch(cmds)
}

async fn color_under_cursor_via_dbus(conn: zbus::Connection) -> Result<(f64, f64, f64), String> {
    let proxy = CosmicCompColorPickerProxy::new(&conn)
        .await
        .map_err(|e| format!("proxy: {e}"))?;
    proxy
        .color_under_cursor()
        .await
        .map_err(|e| format!("color_under_cursor: {e}"))
}
