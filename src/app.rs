use crate::{access, screenshot, subscription};
use cosmic::iced_core::event::wayland::OutputEvent;
use cosmic::{
    app,
    iced::window,
    iced_futures::{event::listen_with, Subscription},
};
use cosmic_client_toolkit::sctk::output::OutputInfo;
use wayland_client::protocol::wl_output::WlOutput;

pub(crate) fn run() -> cosmic::iced::Result {
    let settings = cosmic::app::Settings::default().no_main_window(true);
    cosmic::app::run::<CosmicPortal>(settings, ())
}

#[derive(Default, Clone)]
// run iced app with no main surface
pub struct CosmicPortal {
    pub core: app::Core,

    pub access_args: Option<access::AccessDialogArgs>,
    pub access_choices: Vec<(Option<usize>, Vec<String>)>,

    pub screenshot_args: Option<screenshot::Args>,

    pub outputs: Vec<OutputState>,
    pub prev_rectangle: Option<screenshot::Rect>,
    pub active_output: Option<WlOutput>,
}

#[derive(Debug, Clone)]
pub struct OutputState {
    pub output: WlOutput,
    pub id: window::Id,
    pub info: OutputInfo,
    pub has_pointer: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Access(access::Msg),
    Screenshot(screenshot::Msg),
    Portal(subscription::Event),
    Output(OutputEvent, WlOutput),
}

impl cosmic::Application for CosmicPortal {
    type Executor = cosmic::executor::Default;

    type Flags = ();

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
        _flags: Self::Flags,
    ) -> (Self, cosmic::iced::Command<app::Message<Self::Message>>) {
        (
            Self {
                core,
                ..Default::default()
            },
            cosmic::iced::Command::none(),
        )
    }

    fn view(&self) -> cosmic::prelude::Element<Self::Message> {
        unimplemented!()
    }

    fn view_window(&self, id: window::Id) -> cosmic::prelude::Element<Self::Message> {
        if id == *access::ACCESS_ID {
            access::view(self).map(Msg::Access)
        } else if self.outputs.iter().any(|o| o.id == id) {
            screenshot::view(self, id).map(Msg::Screenshot)
        } else {
            panic!("Unknown window id {:?}", id);
        }
    }

    fn update(
        &mut self,
        message: Self::Message,
    ) -> cosmic::iced::Command<app::Message<Self::Message>> {
        match message {
            Msg::Access(m) => access::update_msg(self, m).map(cosmic::app::Message::App),
            Msg::Portal(e) => match e {
                subscription::Event::Access(args) => {
                    access::update_args(self, args).map(cosmic::app::Message::App)
                }
                subscription::Event::Screenshot(args) => {
                    eprintln!("Updating screenshot args");
                    screenshot::update_args(self, args).map(cosmic::app::Message::App)
                }
            },
            Msg::Screenshot(m) => screenshot::update_msg(self, m).map(cosmic::app::Message::App),
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
                            info,
                            has_pointer: false,
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
                            state.info = info;
                        }
                    }
                    e => {
                        log::warn!("Unhandled output event: {:?} {e:?}", wl_output);
                    }
                };

                if self.prev_rectangle.is_none() {
                    let mut rect = screenshot::Rect::default();
                    for output in &self.outputs {
                        let logical_pos = output.info.logical_position.unwrap_or_default();
                        rect.left = rect.left.min(logical_pos.0);
                        rect.top = rect.top.min(logical_pos.1);
                        let logical_size = output.info.logical_size.unwrap_or_default();
                        rect.right = rect.right.max(logical_pos.0 + logical_size.0);
                        rect.bottom = rect.bottom.max(logical_pos.1 + logical_size.1);
                    }
                    self.prev_rectangle = Some(rect);
                }

                cosmic::iced::Command::none()
            }
        }
    }

    fn subscription(&self) -> cosmic::iced_futures::Subscription<Self::Message> {
        Subscription::batch(vec![
            subscription::portal_subscription().map(|e| Msg::Portal(e)),
            listen_with(|e, _| match e {
                cosmic::iced_core::Event::PlatformSpecific(
                    cosmic::iced_core::event::PlatformSpecific::Wayland(w_e),
                ) => match w_e {
                    cosmic::iced_core::event::wayland::Event::Output(o_event, wl_output) => {
                        Some(Msg::Output(o_event, wl_output))
                    }
                    _ => None,
                },
                cosmic::iced_core::Event::Keyboard(
                    cosmic::iced_core::keyboard::Event::KeyPressed {
                        key_code: cosmic::iced_core::keyboard::KeyCode::Escape,
                        ..
                    },
                ) => Some(Msg::Screenshot(screenshot::Msg::Cancel)),
                _ => None,
            }),
        ])
    }
}
