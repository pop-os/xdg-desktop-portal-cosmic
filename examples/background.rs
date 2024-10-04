// SPDX-License-Identifier: GPL-3.0-only

use ashpd::desktop::background::Background;
use cosmic::{
    app::{self, message, Core},
    executor,
    iced::{Length, Size},
    widget, Command,
};

#[derive(Clone, Debug)]
pub enum Message {
    BackgroundResponse(bool),
    RequestBackground,
}

pub struct App {
    core: Core,
    executable: String,
    background_allowed: bool,
}

impl App {
    async fn request_background(executable: String) -> ashpd::Result<Background> {
        log::info!("Requesting permission to run in the background for: {executable}");
        // Based off of the ashpd docs
        // https://docs.rs/ashpd/latest/ashpd/desktop/background/index.html
        Background::request()
            .reason("Testing the background portal")
            .auto_start(false)
            .dbus_activatable(false)
            .command(&[executable])
            .send()
            .await?
            .response()
    }
}

impl cosmic::Application for App {
    type Executor = executor::single::Executor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "org.cosmic.BackgroundPortalExample";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _: Self::Flags) -> (Self, app::Command<Self::Message>) {
        (
            Self {
                core,
                executable: std::env::args().next().unwrap(),
                background_allowed: false,
            },
            Command::none(),
        )
    }

    fn view(&self) -> cosmic::Element<Self::Message> {
        widget::row::with_children(vec![
            widget::text::title3(if self.background_allowed {
                "Running in background"
            } else {
                "Not running in background"
            })
            .width(Length::Fill)
            .into(),
            widget::button::standard("Run in background")
                .on_press(Message::RequestBackground)
                .padding(8.0)
                .into(),
        ])
        .width(Length::Fill)
        .height(Length::Fixed(64.0))
        .padding(16.0)
        .into()
    }

    fn update(&mut self, message: Self::Message) -> app::Command<Self::Message> {
        match message {
            Message::BackgroundResponse(background_allowed) => {
                log::info!("Permission to run in the background: {background_allowed}");
                self.background_allowed = background_allowed;
                Command::none()
            }
            Message::RequestBackground => {
                let executable = self.executable.clone();
                Command::perform(Self::request_background(executable), |result| {
                    let background_allowed = match result {
                        Ok(response) => {
                            assert!(
                                !response.auto_start(),
                                "Auto start shouldn't have been enabled"
                            );
                            response.run_in_background()
                        }
                        Err(e) => {
                            log::error!("Background portal request failed: {e:?}");
                            false
                        }
                    };

                    message::app(Message::BackgroundResponse(background_allowed))
                })
            }
        }
    }
}

// TODO: Write a small flatpak manifest in order to test this better
#[tokio::main]
async fn main() -> cosmic::iced::Result {
    env_logger::Builder::from_default_env().init();
    let settings = app::Settings::default()
        .resizable(None)
        .size(Size::new(512.0, 128.0))
        .exit_on_close(false);
    app::run::<App>(settings, ())
}
