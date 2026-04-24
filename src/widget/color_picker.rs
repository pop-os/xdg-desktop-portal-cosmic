use cosmic::{
    iced_core::{
        Border, Clipboard, Color, Element, Event, Layout, Length, Point, Rectangle, Renderer as _,
        Shadow, Shell, Size, layout, mouse,
        renderer::{self, Quad},
        widget::Tree,
    },
    widget::Widget,
};

/// Fullscreen transparent overlay for the PickColor portal.
pub struct PickerArea<Message> {
    on_motion: Box<dyn Fn(Point) -> Message>,
    on_click: Box<dyn Fn(Point) -> Message>,
    preview: Option<(f64, f64, f64)>,
}

impl<Message> PickerArea<Message> {
    pub fn new(
        on_motion: impl Fn(Point) -> Message + 'static,
        on_click: impl Fn(Point) -> Message + 'static,
        preview: Option<(f64, f64, f64)>,
    ) -> Self {
        Self {
            on_motion: Box::new(on_motion),
            on_click: Box::new(on_click),
            preview,
        }
    }
}

// Swatch sizing & offset relative to the cursor hotspot.
const SWATCH_SIZE: f32 = 48.0;
const SWATCH_OFFSET_X: f32 = 20.0;
const SWATCH_OFFSET_Y: f32 = 20.0;
const SWATCH_BORDER: f32 = 2.0;

fn plain_quad(bounds: Rectangle, radius: f32) -> Quad {
    Quad {
        bounds,
        border: Border {
            radius: radius.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

fn ring_quad(bounds: Rectangle, radius: f32, thickness: f32, color: Color) -> Quad {
    Quad {
        bounds,
        border: Border {
            radius: radius.into(),
            width: thickness,
            color,
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

impl<Message> Widget<Message, cosmic::Theme, cosmic::Renderer> for PickerArea<Message>
where
    Message: Clone,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(pos) = cursor.position_in(layout.bounds()) {
                    shell.publish((self.on_motion)(pos));
                }
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(pos) = cursor.position_in(layout.bounds()) {
                    shell.publish((self.on_click)(pos));
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        mouse::Interaction::Crosshair
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut cosmic::Renderer,
        _theme: &cosmic::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(cursor_pos) = cursor.position_in(bounds) else {
            return;
        };
        let cx = bounds.x + cursor_pos.x;
        let cy = bounds.y + cursor_pos.y;

        // Position the swatch down-right of the cursor; flip to the opposite
        // side when we'd otherwise run off-screen.
        let flip_x = cx + SWATCH_OFFSET_X + SWATCH_SIZE > bounds.x + bounds.width;
        let flip_y = cy + SWATCH_OFFSET_Y + SWATCH_SIZE > bounds.y + bounds.height;
        let swatch_x = if flip_x {
            cx - SWATCH_OFFSET_X - SWATCH_SIZE
        } else {
            cx + SWATCH_OFFSET_X
        };
        let swatch_y = if flip_y {
            cy - SWATCH_OFFSET_Y - SWATCH_SIZE
        } else {
            cy + SWATCH_OFFSET_Y
        };

        let swatch_bounds = Rectangle::new(
            Point::new(swatch_x, swatch_y),
            Size::new(SWATCH_SIZE, SWATCH_SIZE),
        );

        let fill = match self.preview {
            Some((r, g, b)) => Color::from_rgb(r as f32, g as f32, b as f32),
            None => Color::from_rgba(0.5, 0.5, 0.5, 0.85),
        };

        let radius = 6.0;
        renderer.fill_quad(plain_quad(swatch_bounds, radius), fill);
        renderer.fill_quad(
            ring_quad(
                swatch_bounds,
                radius,
                SWATCH_BORDER,
                Color::from_rgba(0.0, 0.0, 0.0, 0.85),
            ),
            Color::TRANSPARENT,
        );
    }
}

impl<'a, Message> From<PickerArea<Message>>
    for Element<'a, Message, cosmic::Theme, cosmic::Renderer>
where
    Message: 'a + Clone,
{
    fn from(area: PickerArea<Message>) -> Self {
        Element::new(area)
    }
}
