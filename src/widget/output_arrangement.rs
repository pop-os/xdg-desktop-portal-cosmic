//! A container that places output thumbnails at their position in the display
//! arrangement (like the cosmic-settings display page), so side-by-side or
//! stacked screens appear that way. Each child is an ordinary button; this
//! widget handles their placement and draws a name label over each one.

use cosmic::iced::core::renderer::Quad;
use cosmic::iced::core::widget::{Operation, Tree, tree};
use cosmic::iced::core::{
    self as core, Background, Border, Clipboard, Color, Event, Layout, Length, Pixels, Point,
    Rectangle, Renderer as _, Shell, Size, Vector, Widget, alignment, layout, mouse, overlay,
    renderer, text,
};
use cosmic::{Element, Renderer};

const LABEL_BAR_MIN: f32 = 14.0;
const LABEL_BAR_MAX: f32 = 24.0;

pub struct OutputArrangement<'a, Msg> {
    children: Vec<Element<'a, Msg>>,
    regions: Vec<Rectangle>,
    labels: Vec<String>,
    selected: Vec<bool>,
    size: Size,
}

impl<'a, Msg> OutputArrangement<'a, Msg> {
    /// `regions`, `labels` and `selected` must be parallel to `children`. `size` is
    /// the total bounding box of all regions.
    pub fn new(
        children: Vec<Element<'a, Msg>>,
        regions: Vec<Rectangle>,
        labels: Vec<String>,
        selected: Vec<bool>,
        size: Size,
    ) -> Self {
        Self {
            children,
            regions,
            labels,
            selected,
            size,
        }
    }
}

#[derive(Default)]
struct State {
    hovered: Option<usize>,
}

impl<Msg> Widget<Msg, cosmic::Theme, Renderer> for OutputArrangement<'_, Msg> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut self.children);
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fixed(self.size.width),
            height: Length::Fixed(self.size.height),
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        let regions = &self.regions;
        let children = self
            .children
            .iter_mut()
            .enumerate()
            .zip(&mut tree.children)
            .map(|((i, child), child_tree)| {
                let region = regions[i];
                let child_limits = layout::Limits::new(Size::ZERO, region.size());
                child
                    .as_widget_mut()
                    .layout(child_tree, renderer, &child_limits)
                    .move_to(region.position())
            })
            .collect();

        layout::Node::with_children(self.size, children)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &cosmic::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, child_tree), c_layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child.as_widget().draw(
                child_tree,
                renderer,
                theme,
                style,
                c_layout.with_virtual_offset(layout.virtual_offset()),
                cursor,
                viewport,
            );
        }

        // Draw highlights and labels in a new layer so they composite above the
        // thumbnail images. Within a single layer iced always draws quads beneath
        // images, so without it they hide behind the screenshot.
        let accent = theme.cosmic().accent_color();
        let radius = theme.cosmic().radius_s();
        renderer.with_layer(layout.bounds(), |renderer| {
            // Highlight the selected output, and the one under the cursor, so it is
            // clear the thumbnails are clickable.
            for (c_layout, &is_selected) in layout.children().zip(self.selected.iter()) {
                let bounds = c_layout.bounds();
                let hovered = cursor.is_over(bounds);
                if !is_selected && !hovered {
                    continue;
                }

                let mut color = Color::from(accent);
                let border_width = if is_selected { 3.0 } else { 2.0 };
                if !is_selected {
                    color.a = 0.75;
                }

                renderer.fill_quad(
                    Quad {
                        bounds,
                        border: Border {
                            color,
                            width: border_width,
                            radius: radius.into(),
                        },
                        shadow: Default::default(),
                        snap: true,
                    },
                    Background::Color(Color::TRANSPARENT),
                );
            }

            for (c_layout, label) in layout.children().zip(self.labels.iter()) {
                if label.is_empty() {
                    continue;
                }

                let bounds = c_layout.bounds();
                let inset = 4.0;
                let bar_height = (bounds.height * 0.22)
                    .clamp(LABEL_BAR_MIN, LABEL_BAR_MAX)
                    .min(bounds.height - inset);
                let font_size = (bar_height * 0.62).max(9.0);

                // Size the chip to the text (estimated from its length) rather than
                // the whole thumbnail, then center it. Clamp to the thumbnail width.
                let text_width = label.chars().count() as f32 * font_size * 0.55;
                let bar_width = (text_width + font_size * 1.2).min(bounds.width - inset * 2.0);
                let bar = Rectangle {
                    x: bounds.x + (bounds.width - bar_width) / 2.0,
                    y: bounds.y + bounds.height - bar_height - inset,
                    width: bar_width.max(1.0),
                    height: bar_height,
                };

                // Solid rounded chip so the name is legible over any screenshot.
                renderer.fill_quad(
                    Quad {
                        bounds: bar,
                        border: Border {
                            radius: (bar_height / 2.0).into(),
                            ..Default::default()
                        },
                        shadow: Default::default(),
                        snap: true,
                    },
                    Background::Color(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.85,
                    }),
                );

                core::text::Renderer::fill_text(
                    renderer,
                    core::Text {
                        content: label.clone(),
                        size: Pixels(font_size),
                        line_height: text::LineHeight::Relative(1.0),
                        font: cosmic::font::default(),
                        bounds: bar.size(),
                        align_x: text::Alignment::Center,
                        align_y: alignment::Vertical::Center,
                        shaping: text::Shaping::Advanced,
                        wrapping: text::Wrapping::None,
                        ellipsize: text::Ellipsize::None,
                    },
                    Point {
                        x: bar.center_x(),
                        y: bar.center_y(),
                    },
                    Color::WHITE,
                    bar,
                );
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        viewport: &Rectangle,
    ) {
        for ((child, child_tree), c_layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                child_tree,
                event,
                c_layout.with_virtual_offset(layout.virtual_offset()),
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }

        // Track which thumbnail is hovered and request a redraw when it changes, so
        // the hover highlight follows the cursor.
        let hovered = match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                layout.children().position(|l| cursor.is_over(l.bounds()))
            }
            Event::Mouse(mouse::Event::CursorLeft) => None,
            _ => return,
        };
        let state = tree.state.downcast_mut::<State>();
        if state.hovered != hovered {
            state.hovered = hovered;
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, child_tree), c_layout)| {
                child.as_widget().mouse_interaction(
                    child_tree,
                    c_layout.with_virtual_offset(layout.virtual_offset()),
                    cursor,
                    viewport,
                    renderer,
                )
            })
            .max()
            .unwrap_or_default()
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            self.children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
                .for_each(|((child, state), c_layout)| {
                    child.as_widget_mut().operate(
                        state,
                        c_layout.with_virtual_offset(layout.virtual_offset()),
                        renderer,
                        operation,
                    );
                });
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Msg, cosmic::Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Msg: 'static> From<OutputArrangement<'a, Msg>> for Element<'a, Msg> {
    fn from(widget: OutputArrangement<'a, Msg>) -> Self {
        Element::new(widget)
    }
}
