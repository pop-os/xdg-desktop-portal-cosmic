use cosmic::iced::Limits;
use cosmic::iced::core::layout::Node;
use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::widget::Tree;
use cosmic::iced::core::widget::tree::{self, State};
use cosmic::iced::core::{Background, Border, Color, Length, Renderer, Shadow, Size, mouse};
use cosmic::widget::Widget;

pub struct OutputSelection<Msg> {
    on_enter: Msg,
    on_press: Msg,
}

impl<Msg> OutputSelection<Msg> {
    pub fn new(on_enter: Msg, on_press: Msg) -> Self {
        Self { on_enter, on_press }
    }
}

impl<Msg: Clone + 'static> Widget<Msg, cosmic::Theme, cosmic::Renderer> for OutputSelection<Msg> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn state(&self) -> cosmic::iced::core::widget::tree::State {
        State::new(MyState::default())
    }

    fn tag(&self) -> cosmic::iced::core::widget::tree::Tag {
        tree::Tag::of::<MyState>()
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &cosmic::Renderer, limits: &Limits) -> Node {
        let limits = limits.width(Length::Fill).height(Length::Fill);
        Node::new(limits.resolve(Length::Fill, Length::Fill, Size::ZERO))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        _style: &cosmic::iced::core::renderer::Style,
        layout: cosmic::iced::core::Layout<'_>,
        _cursor: cosmic::iced::core::mouse::Cursor,
        _viewport: &cosmic::iced::core::Rectangle,
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
                border: Border {
                    radius: radius_s.into(),
                    width: 12.0,
                    color: accent,
                },
                shadow: Shadow::default(),
                snap: true,
            },
            Background::Color(Color::TRANSPARENT),
        );

        accent.a = 1.0;

        renderer.fill_quad(
            Quad {
                bounds,
                border: Border {
                    radius: radius_s.into(),
                    width: 4.0,
                    color: accent,
                },
                ..Default::default()
            },
            Background::Color(Color::TRANSPARENT),
        );
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: cosmic::iced::core::Layout<'_>,
        cursor: cosmic::iced::core::mouse::Cursor,
        _viewport: &cosmic::iced::core::Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> cosmic::iced::core::mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            cosmic::iced::core::mouse::Interaction::Pointer
        } else {
            cosmic::iced::core::mouse::Interaction::default()
        }
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &cosmic::iced::core::Event,
        layout: cosmic::iced::core::Layout<'_>,
        cursor: cosmic::iced::core::mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn cosmic::iced::core::Clipboard,
        shell: &mut cosmic::iced::core::Shell<'_, Msg>,
        _viewport: &cosmic::iced::core::Rectangle,
    ) {
        // update hover state
        let my_state = state.state.downcast_mut::<MyState>();
        let hovered = cursor.is_over(layout.bounds());
        let changed = my_state.hovered != hovered;
        my_state.hovered = hovered;

        if let cosmic::iced::core::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) =
            event
        {
            shell.publish(self.on_press.clone());
            shell.capture_event();
        }
        if changed
            && let cosmic::iced::core::Event::Mouse(mouse::Event::CursorMoved { .. })
            | cosmic::iced::core::Event::Mouse(mouse::Event::CursorEntered) = event
        {
            shell.publish(self.on_enter.clone());
            shell.capture_event();
        }
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
