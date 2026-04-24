use cosmic::{
    iced::mouse,
    iced_core::{
        self, Background, Border, Color, Length, Point, Rectangle, Renderer, Shadow, Size,
        layout::Node,
        renderer::Quad,
        widget::{
            Tree,
            tree::{self, State},
        },
    },
    widget::Widget,
};

use crate::screenshot::{AnnotationPoint, AnnotationShape, AnnotationTool};

const HANDLE_SIZE: f32 = 8.0;
const LINE_STEP: f32 = 3.0;

pub struct AnnotationCanvas<Msg> {
    annotations: Vec<AnnotationShape>,
    draft: Option<AnnotationShape>,
    tool: AnnotationTool,
    on_draft: Box<dyn Fn(Option<AnnotationShape>) -> Msg>,
    on_commit: Box<dyn Fn(AnnotationShape) -> Msg>,
}

impl<Msg> AnnotationCanvas<Msg> {
    pub fn new(
        annotations: Vec<AnnotationShape>,
        draft: Option<AnnotationShape>,
        tool: AnnotationTool,
        on_draft: impl Fn(Option<AnnotationShape>) -> Msg + 'static,
        on_commit: impl Fn(AnnotationShape) -> Msg + 'static,
    ) -> Self {
        Self {
            annotations,
            draft,
            tool,
            on_draft: Box::new(on_draft),
            on_commit: Box::new(on_commit),
        }
    }

    fn point_at(cursor: mouse::Cursor, bounds: Rectangle) -> Option<AnnotationPoint> {
        let position = cursor.position()?;
        if !bounds.contains(position) {
            return None;
        }

        Some(AnnotationPoint {
            x: ((position.x - bounds.x) / bounds.width).clamp(0.0, 1.0),
            y: ((position.y - bounds.y) / bounds.height).clamp(0.0, 1.0),
        })
    }
}

impl<Msg: Clone + 'static> Widget<Msg, cosmic::Theme, cosmic::Renderer> for AnnotationCanvas<Msg> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn state(&self) -> iced_core::widget::tree::State {
        State::new(CanvasState::default())
    }

    fn tag(&self) -> iced_core::widget::tree::Tag {
        tree::Tag::of::<CanvasState>()
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &cosmic::Renderer,
        limits: &iced_core::layout::Limits,
    ) -> Node {
        Node::new(limits.width(Length::Fill).height(Length::Fill).resolve(
            Length::Fill,
            Length::Fill,
            Size::ZERO,
        ))
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: iced_core::Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &iced_core::Event,
        layout: iced_core::Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn iced_core::Clipboard,
        shell: &mut iced_core::Shell<'_, Msg>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = state.state.downcast_mut::<CanvasState>();

        match event {
            iced_core::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(point) = Self::point_at(cursor, bounds) {
                    state.drag_start = Some(point);
                    shell.publish((self.on_draft)(Some(AnnotationShape::new(
                        self.tool, point, point,
                    ))));
                    shell.capture_event();
                }
            }
            iced_core::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let Some(start) = state.drag_start else {
                    return;
                };
                let Some(end) = Self::point_at(cursor, bounds) else {
                    return;
                };

                shell.publish((self.on_draft)(Some(AnnotationShape::new(
                    self.tool, start, end,
                ))));
                shell.capture_event();
            }
            iced_core::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let Some(start) = state.drag_start.take() else {
                    return;
                };
                let Some(end) = Self::point_at(cursor, bounds) else {
                    shell.publish((self.on_draft)(None));
                    shell.capture_event();
                    return;
                };

                shell.publish((self.on_draft)(None));
                if start.distance(end) > 0.01 {
                    shell.publish((self.on_commit)(AnnotationShape::new(
                        self.tool, start, end,
                    )));
                }
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        _style: &iced_core::renderer::Style,
        layout: iced_core::Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let mut color = Color::from(theme.cosmic().accent_color());
        color.a = 0.95;
        let bounds = layout.bounds();

        for shape in self.annotations.iter().chain(self.draft.iter()) {
            draw_shape(renderer, *shape, bounds, color);
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CanvasState {
    drag_start: Option<AnnotationPoint>,
}

fn draw_shape(
    renderer: &mut cosmic::Renderer,
    shape: AnnotationShape,
    bounds: Rectangle,
    color: Color,
) {
    match shape {
        AnnotationShape::Rectangle { start, end } => {
            draw_rectangle(renderer, start, end, bounds, color)
        }
        AnnotationShape::Arrow { start, end } => draw_arrow(renderer, start, end, bounds, color),
    }
}

fn draw_rectangle(
    renderer: &mut cosmic::Renderer,
    start: AnnotationPoint,
    end: AnnotationPoint,
    bounds: Rectangle,
    color: Color,
) {
    let start = to_screen(start, bounds);
    let end = to_screen(end, bounds);
    let x = start.x.min(end.x);
    let y = start.y.min(end.y);
    let width = (start.x - end.x).abs();
    let height = (start.y - end.y).abs();

    renderer.fill_quad(
        Quad {
            bounds: Rectangle::new(Point::new(x, y), Size::new(width, height)),
            border: Border {
                radius: 0.0.into(),
                width: 3.0,
                color,
            },
            shadow: Shadow::default(),
            snap: true,
        },
        Background::Color(Color::TRANSPARENT),
    );
}

fn draw_arrow(
    renderer: &mut cosmic::Renderer,
    start: AnnotationPoint,
    end: AnnotationPoint,
    bounds: Rectangle,
    color: Color,
) {
    let start = to_screen(start, bounds);
    let end = to_screen(end, bounds);
    draw_line(renderer, start, end, color);

    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let angle = dy.atan2(dx);
    let head_len = 22.0;
    let spread = 0.55;

    for head_angle in [
        angle + std::f32::consts::PI - spread,
        angle + std::f32::consts::PI + spread,
    ] {
        let head = Point::new(
            end.x + head_len * head_angle.cos(),
            end.y + head_len * head_angle.sin(),
        );
        draw_line(renderer, end, head, color);
    }
}

fn draw_line(renderer: &mut cosmic::Renderer, start: Point, end: Point, color: Color) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let steps = (dx.hypot(dy) / LINE_STEP).ceil().max(1.0) as u32;

    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let point = Point::new(start.x + dx * t, start.y + dy * t);
        renderer.fill_quad(
            Quad {
                bounds: Rectangle::new(
                    Point::new(point.x - HANDLE_SIZE / 2.0, point.y - HANDLE_SIZE / 2.0),
                    Size::new(HANDLE_SIZE, HANDLE_SIZE),
                ),
                border: Border {
                    radius: (HANDLE_SIZE / 2.0).into(),
                    ..Default::default()
                },
                shadow: Shadow::default(),
                snap: true,
            },
            Background::Color(color),
        );
    }
}

fn to_screen(point: AnnotationPoint, bounds: Rectangle) -> Point {
    Point::new(
        bounds.x + point.x * bounds.width,
        bounds.y + point.y * bounds.height,
    )
}

impl<'a, Message> From<AnnotationCanvas<Message>> for cosmic::Element<'a, Message>
where
    Message: 'static + Clone,
{
    fn from(w: AnnotationCanvas<Message>) -> cosmic::Element<'a, Message> {
        cosmic::Element::new(w)
    }
}
