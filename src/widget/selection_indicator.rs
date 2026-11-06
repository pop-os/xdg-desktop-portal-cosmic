//! The badge that marks a capture source as selectable and selected

use cosmic::iced::core::widget::Tree;
use cosmic::iced::core::{
    Background, Border, Color, Layout, Length, Rectangle, Renderer as _, Size, Widget, layout,
    mouse, renderer, svg,
};
use cosmic::widget::button::Catalog as _;
use cosmic::{Element, Renderer};
use std::sync::LazyLock;

/// Chip behind the mark, sized as in libcosmic's image button.
const CHIP: Size = Size::new(24.0, 20.0);
/// The mark itself: a check icon, or an empty box or circle.
const MARK: f32 = 16.0;
/// Keeps the badge clear of the thumbnail's selection border.
const INSET: f32 = 3.0;
/// Dot inside a selected radio button, in libcosmic's 6-in-20 proportion.
const DOT: f32 = MARK * 0.3;

static OBJECT_SELECT: LazyLock<svg::Handle> = LazyLock::new(|| {
    cosmic::widget::icon::from_name("object-select-symbolic")
        .size(MARK as u16)
        .icon()
        .into_svg_handle()
        .unwrap_or_else(|| {
            let bytes: &'static [u8] = &[];
            svg::Handle::from_memory(bytes)
        })
});

/// Draw the badge in the top left of `thumbnail`. Top, because output
/// thumbnails carry their name along the bottom.
pub fn draw_badge(
    renderer: &mut Renderer,
    theme: &cosmic::Theme,
    thumbnail: Rectangle,
    selected: bool,
    multiple: bool,
) {
    // Never scale with the thumbnail
    if thumbnail.width < CHIP.width * 2.0 || thumbnail.height < CHIP.height * 2.0 {
        return;
    }

    let radius = theme.cosmic().radius_s();
    let chip = Rectangle {
        x: thumbnail.x + INSET,
        y: thumbnail.y + INSET,
        width: CHIP.width,
        height: CHIP.height,
    };

    renderer.fill_quad(
        renderer::Quad {
            bounds: chip,
            border: Border {
                radius: [radius[0], 0.0, radius[2], 0.0].into(),
                ..Default::default()
            },
            shadow: Default::default(),
            snap: true,
        },
        theme.selection_background(),
    );

    draw_mark(
        renderer,
        theme,
        Rectangle {
            x: chip.center_x() - MARK / 2.0,
            y: chip.center_y() - MARK / 2.0,
            width: MARK,
            height: MARK,
        },
        selected,
        multiple,
    );
}

fn draw_mark(
    renderer: &mut Renderer,
    theme: &cosmic::Theme,
    bounds: Rectangle,
    selected: bool,
    multiple: bool,
) {
    let cosmic = theme.cosmic();

    if multiple {
        if selected {
            let icon =
                svg::Svg::new(OBJECT_SELECT.clone()).color(Color::from(cosmic.accent_color()));
            svg::Renderer::draw_svg(renderer, icon, bounds, bounds);
        } else {
            draw_outline(renderer, bounds, cosmic.radius_xs(), cosmic);
        }
        return;
    }

    if !selected {
        draw_outline(renderer, bounds, [MARK / 2.0; 4], cosmic);
        return;
    }

    renderer.fill_quad(
        renderer::Quad {
            bounds,
            border: Border {
                radius: (MARK / 2.0).into(),
                ..Default::default()
            },
            shadow: Default::default(),
            snap: true,
        },
        Background::Color(Color::from(cosmic.accent_color())),
    );
    renderer.fill_quad(
        renderer::Quad {
            bounds: Rectangle {
                x: bounds.center_x() - DOT / 2.0,
                y: bounds.center_y() - DOT / 2.0,
                width: DOT,
                height: DOT,
            },
            border: Border {
                radius: (DOT / 2.0).into(),
                ..Default::default()
            },
            shadow: Default::default(),
            snap: true,
        },
        Background::Color(Color::from(cosmic.on_accent_color())),
    );
}

/// An empty box or circle
fn draw_outline(
    renderer: &mut Renderer,
    bounds: Rectangle,
    radius: [f32; 4],
    cosmic: &cosmic::cosmic_theme::Theme,
) {
    let mut color = Color::from(cosmic.on_primary_container_color());
    color.a *= 0.7;

    renderer.fill_quad(
        renderer::Quad {
            bounds,
            border: Border {
                color,
                width: 1.5,
                radius: radius.into(),
            },
            shadow: Default::default(),
            snap: true,
        },
        Background::Color(Color::TRANSPARENT),
    );
}

/// The mark on its own, without the chip, for laying out beside a label
/// whatever contains it is responsible for the click.
pub struct SelectionIndicator {
    selected: bool,
    multiple: bool,
}

impl SelectionIndicator {
    pub fn new(selected: bool, multiple: bool) -> Self {
        Self { selected, multiple }
    }
}

impl<Msg> Widget<Msg, cosmic::Theme, Renderer> for SelectionIndicator {
    fn size(&self) -> Size<Length> {
        Size {
            width: Length::Fixed(MARK),
            height: Length::Fixed(MARK),
        }
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(Size::new(MARK, MARK))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &cosmic::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        draw_mark(
            renderer,
            theme,
            layout.bounds(),
            self.selected,
            self.multiple,
        );
    }
}

impl<'a, Msg: 'a> From<SelectionIndicator> for Element<'a, Msg> {
    fn from(indicator: SelectionIndicator) -> Self {
        Element::new(indicator)
    }
}
