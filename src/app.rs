use crate::{access, config, file_chooser, screencast_dialog, screenshot, subscription};
use cosmic::Task;
use cosmic::iced_core::event::wayland::OutputEvent;
use cosmic::widget;
use cosmic::{
    app, cosmic_config,
    iced::window,
    iced_futures::{Subscription, event::listen_with},
};
use std::collections::HashMap;
use wayland_client::protocol::wl_output::WlOutput;

pub(crate) fn run() -> cosmic::iced::Result {
    let settings = cosmic::app::Settings::default()
        .no_main_window(true)
        .exit_on_close(false);
    let (config, config_handler) = config::Config::load();
    let flags = Flags {
        config,
        config_handler,
    };
    cosmic::app::run::<CosmicPortal>(settings, flags)
}

// run iced app with no main surface
pub struct CosmicPortal {
    pub core: app::Core,
    pub tx: Option<tokio::sync::mpsc::Sender<subscription::Event>>,

    pub config_handler: Option<cosmic_config::Config>,
    pub config: config::Config,

    pub access_args: Option<access::AccessDialogArgs>,

    pub file_choosers: HashMap<window::Id, (file_chooser::Args, file_chooser::Dialog)>,

    pub screenshot_args: Option<screenshot::Args>,
    pub screencast_args: Option<screencast_dialog::Args>,
    pub screencast_tab_model:
        widget::segmented_button::Model<widget::segmented_button::SingleSelect>,
    pub location_options: Vec<String>,
    pub prev_rectangle: Option<screenshot::Rect>,
    pub wayland_helper: crate::wayland::WaylandHelper,

    pub outputs: Vec<OutputState>,
    pub active_output: Option<WlOutput>,
}

#[derive(Debug, Clone)]
pub struct OutputState {
    pub output: WlOutput,
    pub id: window::Id,
    pub name: String,
    pub logical_size: (u32, u32),
    pub logical_pos: (i32, i32),
    pub has_pointer: bool,
    pub bg_source: Option<cosmic_bg_config::Source>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Access(access::Msg),
    FileChooser(window::Id, file_chooser::Msg),
    Screenshot(screenshot::Msg),
    Screencast(screencast_dialog::Msg),
    Portal(subscription::Event),
    Output(OutputEvent, WlOutput),
    ConfigSetScreenshot(config::screenshot::Screenshot),
    /// Update config from external changes
    ConfigSubUpdate(config::Config),
}

#[derive(Clone, Debug)]
pub struct Flags {
    pub config_handler: Option<cosmic_config::Config>,
    pub config: config::Config,
}

impl cosmic::Application for CosmicPortal {
    type Executor = cosmic::executor::Default;

    type Flags = Flags;

    type Message = Msg;

    const APP_ID: &'static str = "org.freedesktop.portal.desktop.cosmic";

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(
        core: app::Core,
        Flags {
            config_handler,
            config,
        }: Self::Flags,
    ) -> (Self, cosmic::iced::Task<cosmic::Action<Self::Message>>) {
        let wayland_conn = crate::wayland::connect_to_wayland();
        let wayland_helper = crate::wayland::WaylandHelper::new(wayland_conn);
        (
            Self {
                core,
                config_handler,
                config,
                access_args: Default::default(),
                file_choosers: Default::default(),
                screenshot_args: Default::default(),
                screencast_args: Default::default(),
                screencast_tab_model: Default::default(),
                location_options: Vec::new(),
                prev_rectangle: Default::default(),
                outputs: Default::default(),
                active_output: Default::default(),
                wayland_helper,
                tx: None,
            },
            cosmic::iced::Task::none(),
        )
    }

    fn view(&self) -> cosmic::Element<'_, Self::Message> {
        unimplemented!()
    }

    fn view_window(&self, id: window::Id) -> cosmic::Element<'_, Self::Message> {
        if Some(id) == self.access_args.as_ref().map(|args| args.access_id) {
            access::view(self).map(Msg::Access)
        } else if id == *screencast_dialog::SCREENCAST_ID {
            screencast_dialog::view(self).map(Msg::Screencast)
        } else if self.outputs.iter().any(|o| o.id == id) {
            screenshot::view(self, id).map(Msg::Screenshot)
        } else {
            file_chooser::view(self, id)
        }
    }

    fn update(
        &mut self,
        message: Self::Message,
    ) -> cosmic::iced::Task<cosmic::Action<Self::Message>> {
        match message {
            Msg::Access(m) => access::update_msg(self, m).map(cosmic::Action::App),
            Msg::FileChooser(id, m) => file_chooser::update_msg(self, id, m),
            Msg::Portal(e) => match e {
                subscription::Event::Access(args) => {
                    access::update_args(self, args).map(cosmic::Action::App)
                }
                subscription::Event::FileChooser(args) => file_chooser::update_args(self, args),
                subscription::Event::Screenshot(args) => {
                    screenshot::update_args(self, args).map(cosmic::Action::App)
                }
                subscription::Event::Screencast(args) => {
                    screencast_dialog::update_args(self, args).map(cosmic::Action::App)
                }
                subscription::Event::CancelScreencast(handle) => {
                    screencast_dialog::cancel(self, handle).map(cosmic::Action::App)
                }
                subscription::Event::Config(config) => self.update(Msg::ConfigSubUpdate(config)),
                subscription::Event::Accent(_)
                | subscription::Event::IsDark(_)
                | subscription::Event::HighContrast(_)
                | subscription::Event::ButtonPlacement(_) => cosmic::iced::Task::none(),
                subscription::Event::Init(tx) => {
                    self.tx = Some(tx);
                    Task::none()
                }
            },
            Msg::Screenshot(m) => screenshot::update_msg(self, m).map(cosmic::Action::App),
            Msg::Screencast(m) => screencast_dialog::update_msg(self, m).map(cosmic::Action::App),
            Msg::Output(o_event, wl_output) => {
                match o_event {
                    OutputEvent::Created(Some(info))
                        if info.name.is_some()
                            && info.logical_size.is_some()
                            && info.logical_position.is_some() =>
                    {
                        self.outputs.push(OutputState {
                            output: wl_output,
                            id: window::Id::unique(),
                            name: info.name.unwrap(),
                            logical_size: info
                                .logical_size
                                .map(|(w, h)| (w as u32, h as u32))
                                .unwrap(),
                            logical_pos: info.logical_position.unwrap(),
                            has_pointer: false,
                            bg_source: None,
                        })
                    }
                    OutputEvent::Removed => self.outputs.retain(|o| o.output != wl_output),
                    OutputEvent::InfoUpdate(info)
                        if info.name.is_some()
                            && info.logical_size.is_some()
                            && info.logical_position.is_some() =>
                    {
                        if let Some(state) = self.outputs.iter_mut().find(|o| o.output == wl_output)
                        {
                            state.name = info.name.unwrap();
                            state.logical_size = info
                                .logical_size
                                .map(|(w, h)| (w as u32, h as u32))
                                .unwrap();
                            state.logical_pos = info.logical_position.unwrap();
                        } else {
                            log::warn!("Updated output {:?} not found", wl_output);
                            self.outputs.push(OutputState {
                                output: wl_output,
                                id: window::Id::unique(),
                                name: info.name.unwrap(),
                                logical_size: info
                                    .logical_size
                                    .map(|(w, h)| (w as u32, h as u32))
                                    .unwrap(),
                                logical_pos: info.logical_position.unwrap(),
                                has_pointer: false,
                                bg_source: None,
                            });
                        }
                    }
                    e => {
                        log::warn!("Unhandled output event: {:?} {e:?}", wl_output);
                    }
                };

                if self.prev_rectangle.is_none() {
                    let mut rect = screenshot::Rect::default();
                    for output in &self.outputs {
                        rect.left = rect.left.min(output.logical_pos.0);
                        rect.top = rect.top.min(output.logical_pos.1);
                        rect.right = rect
                            .right
                            .max(output.logical_pos.0 + output.logical_size.0 as i32);
                        rect.bottom = rect
                            .bottom
                            .max(output.logical_pos.1 + output.logical_size.1 as i32);
                    }
                    self.prev_rectangle = Some(rect);
                }

                cosmic::iced::Task::none()
            }
            Msg::ConfigSetScreenshot(screenshot) => {
                match &mut self.config_handler {
                    Some(handler) => {
                        if let Err(e) = self.config.set_screenshot(handler, screenshot) {
                            log::error!("Failed to save screenshot config: {e}")
                        }
                    }
                    None => log::error!("Failed to save config: No config handler"),
                }

                cosmic::iced::Task::none()
            }
            Msg::ConfigSubUpdate(config) => {
                self.config = config;
                cosmic::iced::Task::none()
            }
        }
    }

    #[allow(clippy::collapsible_match)]
    fn subscription(&self) -> cosmic::iced_futures::Subscription<Self::Message> {
        let mut subscriptions = vec![
            subscription::portal_subscription(self.wayland_helper.clone()).map(Msg::Portal),
            listen_with(|e, _, _| match e {
                cosmic::iced_core::Event::PlatformSpecific(
                    cosmic::iced_core::event::PlatformSpecific::Wayland(w_e),
                ) => match w_e {
                    cosmic::iced_core::event::wayland::Event::Output(o_event, wl_output) => {
                        Some(Msg::Output(o_event, wl_output))
                    }
                    _ => None,
                },
                _ => None,
            }),
        ];
        for (id, (_args, dialog)) in self.file_choosers.iter() {
            let id = id.clone();
            subscriptions.push(dialog.subscription().map(move |x| Msg::FileChooser(id, x)));
        }
        Subscription::batch(subscriptions)
    }

    fn system_theme_mode_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &cosmic::cosmic_theme::ThemeMode,
    ) -> app::Task<Self::Message> {
        let old = self.core.system_is_dark();
        let new = new_theme.is_dark;
        if new != old {
            if let Some(tx) = self.tx.clone() {
                tokio::spawn(async move {
                    _ = tx.send(subscription::Event::IsDark(new)).await;
                });
            }
        }
        Task::none()
    }

    fn system_theme_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &cosmic::cosmic_theme::Theme,
    ) -> cosmic::iced::Task<cosmic::Action<Self::Message>> {
        let old = self.core.system_theme().cosmic();
        let mut msgs = Vec::with_capacity(3);

        if old.is_dark != new_theme.is_dark {
            return Task::none();
        }

        if old.accent_color() != new_theme.accent_color() {
            msgs.push(subscription::Event::Accent(new_theme.accent_color()));
        }
        if old.is_high_contrast != new_theme.is_high_contrast {
            msgs.push(subscription::Event::HighContrast(
                new_theme.is_high_contrast,
            ));
        }
        // TODO: only send if value actually changed
        msgs.push(subscription::Event::ButtonPlacement(
            crate::Settings::new().button_placement,
        ));
        {
            if let Some(tx) = self.tx.clone() {
                tokio::spawn(async move {
                    for msg in msgs {
                        _ = tx.send(msg).await;
                    }
                });
            }
        }
        Task::none()
    }
}
