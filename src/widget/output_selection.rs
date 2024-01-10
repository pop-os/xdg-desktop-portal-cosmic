use cosmic::{
    iced::Limits,
    iced_core::{
        layout::Node,
        mouse,
        renderer::Quad,
        widget::{
            tree::{self, State},
            Tree,
        },
        Background, Color, Length, Renderer, Size,
    },
    widget::Widget,
};

pub struct OutputSelection<Msg> {
    on_enter: Msg,
}

impl<Msg> OutputSelection<Msg> {
    pub fn new(on_enter: Msg) -> Self {
        Self { on_enter }
    }
}

impl<Msg: Clone + 'static> Widget<Msg, cosmic::Renderer> for OutputSelection<Msg> {
    fn width(&self) -> Length {
        Length::Fill
    }

    fn height(&self) -> Length {
        Length::Fill
    }

    fn state(&self) -> cosmic::iced_core::widget::tree::State {
        State::new(MyState::default())
    }

    fn tag(&self) -> cosmic::iced_core::widget::tree::Tag {
        tree::Tag::of::<MyState>()
    }

    fn layout(&self, _tree: &mut Tree, _renderer: &cosmic::Renderer, limits: &Limits) -> Node {
        let limits = limits.width(Length::Fill).height(Length::Fill);
        Node::new(limits.resolve(Size::ZERO))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &<cosmic::Renderer as cosmic::iced_core::Renderer>::Theme,
        _style: &cosmic::iced_core::renderer::Style,
        layout: cosmic::iced_core::Layout<'_>,
        _cursor: cosmic::iced_core::mouse::Cursor,
        _viewport: &cosmic::iced_core::Rectangle,
    ) {
        let cosmic = theme.cosmic();
        let radius_s = cosmic.radius_s();
        let mut accent = Color::from(cosmic.accent_color());
        // draw two rectangles if hovered
        let should_draw = {
            let my_state = tree.state.downcast_ref::<MyState>();
            my_state.hovered || my_state.focused
        };

        if !should_draw {
            return;
        }

        let bounds = layout.bounds();
        accent.a = 0.7;
        renderer.fill_quad(
            Quad {
                bounds,
                border_radius: radius_s.into(),
                border_width: 12.0,
                border_color: accent,
            },
            Background::Color(Color::TRANSPARENT),
        );

        accent.a = 1.0;

        renderer.fill_quad(
            Quad {
                bounds,
                border_radius: radius_s.into(),
                border_width: 4.0,
                border_color: accent,
            },
            Background::Color(Color::TRANSPARENT),
        );
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: cosmic::iced_core::Layout<'_>,
        cursor: cosmic::iced_core::mouse::Cursor,
        _viewport: &cosmic::iced_core::Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> cosmic::iced_core::mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            cosmic::iced_core::mouse::Interaction::Pointer
        } else {
            cosmic::iced_core::mouse::Interaction::default()
        }
    }

    fn on_event(
        &mut self,
        state: &mut Tree,
        event: cosmic::iced_core::Event,
        layout: cosmic::iced_core::Layout<'_>,
        cursor: cosmic::iced_core::mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn cosmic::iced_core::Clipboard,
        shell: &mut cosmic::iced_core::Shell<'_, Msg>,
        _viewport: &cosmic::iced_core::Rectangle,
    ) -> cosmic::iced_core::event::Status {
        // update hover state
        let my_state = state.state.downcast_mut::<MyState>();
        let hovered = cursor.is_over(layout.bounds());
        let changed = my_state.hovered != hovered;
        my_state.hovered = hovered;

        if changed {
            return match event {
                cosmic::iced_core::Event::Mouse(mouse::Event::CursorMoved { .. })
                | cosmic::iced_core::Event::Mouse(mouse::Event::CursorEntered) => {
                    shell.publish(self.on_enter.clone());
                    cosmic::iced_core::event::Status::Captured
                }
                _ => cosmic::iced_core::event::Status::Ignored,
            };
        }
        cosmic::iced_core::event::Status::Ignored
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MyState {
    pub hovered: bool,
    pub focused: bool,
}

impl<'a, Message> From<OutputSelection<Message>> for cosmic::Element<'a, Message>
where
    Message: 'static + Clone,
{
    fn from(w: OutputSelection<Message>) -> cosmic::Element<'a, Message> {
        cosmic::Element::new(w)
    }
}
