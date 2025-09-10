use cosmic::iced_core::{
    Clipboard, Element, Layout, Length, Rectangle, Shell, Size, Widget,
    event::{self, Event},
    keyboard, layout, mouse, overlay, renderer,
    widget::{Operation, Tree},
};

#[allow(missing_debug_implementations)]
pub struct KeyboardWrapper<'a, Message> {
    content: Element<'a, Message, cosmic::Theme, cosmic::Renderer>,
    handler: fn(keyboard::Key) -> Option<Message>,
}

impl<'a, Message> KeyboardWrapper<'a, Message> {
    /// Creates a [`KeyboardWrapper`] with the given content.
    pub fn new(
        content: impl Into<Element<'a, Message, cosmic::Theme, cosmic::Renderer>>,
        handler: fn(keyboard::Key) -> Option<Message>,
    ) -> Self {
        KeyboardWrapper {
            content: content.into(),
            handler,
        }
    }
}

impl<'a, Message> Widget<Message, cosmic::Theme, cosmic::Renderer> for KeyboardWrapper<'a, Message>
where
    Message: Clone,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation<()>,
    ) {
        self.content
            .as_widget()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        if let event::Status::Captured = self.content.as_widget_mut().on_event(
            &mut tree.children[0],
            event.clone(),
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        ) {
            return event::Status::Captured;
        }

        match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => {
                if let Some(message) = (self.handler)(key) {
                    shell.publish(message.clone());
                    event::Status::Captured
                } else {
                    event::Status::Ignored
                }
            }
            /*
                keyboard::key::Named::Escape => {
                    event::Status::Ignored
                }
                keyboard::key::Named::Enter => {
                    event::Status::Ignored
                }
                _ => event::Status::Ignored
            },
            */
            _ => event::Status::Ignored,
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        renderer_style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            renderer_style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        translation: cosmic::iced::Vector,
    ) -> Option<overlay::Element<'b, Message, cosmic::Theme, cosmic::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, translation)
    }

    fn drag_destinations(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        dnd_rectangles: &mut cosmic::iced_core::clipboard::DndDestinationRectangles,
    ) {
        if let Some(state) = state.children.iter().next() {
            self.content
                .as_widget()
                .drag_destinations(state, layout, renderer, dnd_rectangles);
        }
    }
}

impl<'a, Message> From<KeyboardWrapper<'a, Message>>
    for Element<'a, Message, cosmic::Theme, cosmic::Renderer>
where
    Message: 'a + Clone,
{
    fn from(
        area: KeyboardWrapper<'a, Message>,
    ) -> Element<'a, Message, cosmic::Theme, cosmic::Renderer> {
        Element::new(area)
    }
}
