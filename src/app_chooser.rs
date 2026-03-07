use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::window;
use cosmic::iced_runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced_winit::commands::layer_surface::{destroy_layer_surface, get_layer_surface};
use cosmic::widget::autosize::autosize;
use cosmic::widget::{self, Id, button, icon, text};
use cosmic::{
    iced::widget::{column, scrollable},
    iced_core::Alignment,
};
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::app::CosmicPortal;
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use crate::{PortalResponse, fl, subscription};

#[allow(dead_code)]
#[derive(zvariant::DeserializeDict, zvariant::Type, Debug, Clone)]
#[zvariant(signature = "a{sv}")]
struct ChooseApplicationOptions {
    last_choice: Option<String>,
    modal: Option<bool>,
    content_type: Option<String>,
    uri: Option<String>,
    filename: Option<String>,
    activation_token: Option<String>,
}

#[derive(zvariant::SerializeDict, zvariant::Type, Debug)]
#[zvariant(signature = "a{sv}")]
pub(crate) struct ChooseApplicationResult {
    choice: String,
}

pub struct AppChooser {
    tx: Sender<subscription::Event>,
}

impl AppChooser {
    pub fn new(tx: Sender<subscription::Event>) -> Self {
        Self { tx }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.AppChooser")]
impl AppChooser {
    async fn choose_application(
        &self,
        _handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        _parent_window: &str,
        choices: Vec<String>,
        options: ChooseApplicationOptions,
    ) -> PortalResponse<ChooseApplicationResult> {
        log::debug!(
            "AppChooser::choose_application app_id={app_id} choices={choices:?} content_type={:?} uri={:?}",
            options.content_type,
            options.uri
        );

        if choices.is_empty() {
            log::warn!("AppChooser: no choices available");
            return PortalResponse::Cancelled;
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        let args = AppChooserArgs {
            choices,
            last_choice: options.last_choice,
            content_type: options.content_type,
            uri: options.uri,
            filename: options.filename,
            modal: options.modal.unwrap_or_default(),
            tx,
            surface_id: window::Id::NONE,
            selected: None,
        };

        if let Err(err) = self.tx.send(subscription::Event::AppChooser(args)).await {
            log::error!("Failed to send app chooser event: {err}");
            return PortalResponse::Other;
        }

        if let Some(res) = rx.recv().await {
            res
        } else {
            PortalResponse::Cancelled
        }
    }

    async fn update_choices(
        &self,
        _handle: zvariant::ObjectPath<'_>,
        choices: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        log::debug!("AppChooser::update_choices choices={choices:?}");
        // TODO: update dialog choices if open
        Ok(())
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

// Messages for the UI

#[derive(Debug, Clone)]
pub enum Msg {
    Select(usize),
    Open,
    Cancel,
    Ignore,
}

// Args passed from D-Bus handler to UI

#[derive(Clone, Debug)]
pub struct AppChooserArgs {
    pub choices: Vec<String>,
    pub last_choice: Option<String>,
    pub content_type: Option<String>,
    pub uri: Option<String>,
    pub filename: Option<String>,
    pub modal: bool,
    pub tx: Sender<PortalResponse<ChooseApplicationResult>>,
    pub surface_id: window::Id,
    pub selected: Option<usize>,
}

impl AppChooserArgs {
    pub(crate) fn get_surface(&mut self) -> cosmic::Task<Msg> {
        if self.modal {
            let (id, task) = window::open(window::Settings {
                resizable: false,
                ..Default::default()
            });
            self.surface_id = id;
            task.map(|_| Msg::Ignore)
        } else {
            self.surface_id = window::Id::unique();
            get_layer_surface(SctkLayerSurfaceSettings {
                id: self.surface_id,
                layer: cosmic_client_toolkit::sctk::shell::wlr_layer::Layer::Top,
                keyboard_interactivity:
                    cosmic_client_toolkit::sctk::shell::wlr_layer::KeyboardInteractivity::OnDemand,
                input_zone: None,
                anchor: cosmic_client_toolkit::sctk::shell::wlr_layer::Anchor::empty(),
                output: IcedOutput::Active,
                namespace: "app-chooser portal".to_string(),
                ..Default::default()
            })
        }
    }

    pub(crate) fn destroy_surface(&self) -> cosmic::Task<Msg> {
        if self.modal {
            window::close(self.surface_id)
        } else {
            destroy_layer_surface(self.surface_id)
        }
    }
}

struct AppInfo {
    name: String,
    icon_name: String,
}

fn lookup_app_info(app_id: &str) -> AppInfo {
    use freedesktop_desktop_entry as fde;

    let fallback_name = app_id.split('.').next_back().unwrap_or(app_id).to_string();

    for path in fde::Iter::new(fde::default_paths()) {
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(entry) = fde::DesktopEntry::from_str(&path, &data, Some(&["en"])) else {
            continue;
        };
        if entry.id() == app_id
            || path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s == app_id)
        {
            return AppInfo {
                name: entry
                    .name::<&str>(&[])
                    .map(|n| n.into_owned())
                    .unwrap_or_else(|| fallback_name.clone()),
                icon_name: entry
                    .icon()
                    .unwrap_or("application-x-executable")
                    .to_string(),
            };
        }
    }

    AppInfo {
        name: fallback_name,
        icon_name: "application-x-executable".to_string(),
    }
}

// View

pub(crate) fn view(portal: &CosmicPortal) -> cosmic::Element<'_, Msg> {
    let spacing = portal.core.system_theme().cosmic().spacing;
    let Some(args) = portal.app_chooser_args.as_ref() else {
        return text("").into();
    };

    let title = if let Some(name) = args
        .filename
        .as_deref()
        .or(args.uri.as_deref())
        .or(args.content_type.as_deref())
    {
        fl!("open-item-with", name = name)
    } else {
        fl!("open-with")
    };

    let mut app_list = Vec::with_capacity(args.choices.len());
    for (i, app_id) in args.choices.iter().enumerate() {
        let info = lookup_app_info(app_id);
        let is_selected = args.selected == Some(i);

        let row_icon = icon::Icon::from(icon::from_name(info.icon_name).size(32));

        let row = cosmic::iced::widget::row![row_icon, text(info.name)]
            .spacing(spacing.space_s as f32)
            .align_y(Alignment::Center);

        let btn = if is_selected {
            button::custom(row).class(cosmic::theme::Button::Suggested)
        } else {
            button::custom(row)
        }
        .width(cosmic::iced::Length::Fill)
        .on_press(Msg::Select(i));

        app_list.push(btn.into());
    }

    let list = scrollable(
        cosmic::widget::Column::with_children(app_list)
            .spacing(spacing.space_xxs as f32)
            .align_x(Alignment::Start),
    )
    .height(cosmic::iced::Length::Shrink);

    let cancel_button = button::text(fl!("cancel")).on_press(Msg::Cancel);

    let open_button = if args.selected.is_some() {
        button::text(fl!("open"))
            .on_press(Msg::Open)
            .class(cosmic::theme::Button::Suggested)
    } else {
        button::text(fl!("open")).class(cosmic::theme::Button::Suggested)
    };

    let content = KeyboardWrapper::new(
        widget::dialog()
            .title(title)
            .control(column![list].spacing(spacing.space_s as f32))
            .secondary_action(cancel_button)
            .primary_action(open_button),
        |key, _| match key {
            Key::Named(Named::Enter) => Some(Msg::Open),
            Key::Named(Named::Escape) => Some(Msg::Cancel),
            _ => None,
        },
    );

    autosize(content, Id::new("app-chooser-dialog"))
        .min_width(1.)
        .min_height(1.)
        .into()
}

// Update

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    match msg {
        Msg::Select(i) => {
            if let Some(args) = portal.app_chooser_args.as_mut() {
                args.selected = Some(i);
            }
            cosmic::iced::Task::none()
        }
        Msg::Open => {
            if let Some(args) = portal.app_chooser_args.take() {
                if let Some(i) = args.selected
                    && let Some(choice) = args.choices.get(i).cloned()
                {
                    let tx = args.tx.clone();
                    tokio::spawn(async move {
                        let _ = tx
                            .send(PortalResponse::Success(ChooseApplicationResult { choice }))
                            .await;
                    });
                    return args.destroy_surface().map(crate::app::Msg::AppChooser);
                }
                // No selection — put args back
                portal.app_chooser_args = Some(args);
            }
            cosmic::iced::Task::none()
        }
        Msg::Cancel => {
            if let Some(args) = portal.app_chooser_args.take() {
                let tx = args.tx.clone();
                tokio::spawn(async move {
                    let _ = tx
                        .send(PortalResponse::Cancelled::<ChooseApplicationResult>)
                        .await;
                });
                return args.destroy_surface().map(crate::app::Msg::AppChooser);
            }
            cosmic::iced::Task::none()
        }
        Msg::Ignore => cosmic::iced::Task::none(),
    }
    .map(crate::app::Msg::AppChooser)
}

pub fn update_args(
    portal: &mut CosmicPortal,
    mut args: AppChooserArgs,
) -> cosmic::Task<crate::app::Msg> {
    let mut cmds = Vec::with_capacity(2);

    // Cancel existing dialog if any
    if let Some(prev) = portal.app_chooser_args.take() {
        cmds.push(prev.destroy_surface());
        tokio::spawn(async move {
            let _ = prev
                .tx
                .send(PortalResponse::Cancelled::<ChooseApplicationResult>)
                .await;
        });
    }

    // Pre-select last_choice if it's in the list
    if let Some(ref last) = args.last_choice {
        args.selected = args.choices.iter().position(|c| c == last);
    }

    cmds.push(args.get_surface());
    portal.app_chooser_args = Some(args);
    cosmic::iced::Task::batch(cmds).map(crate::app::Msg::AppChooser)
}
