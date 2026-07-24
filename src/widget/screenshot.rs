use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use cosmic::Element;
use cosmic::cosmic_theme::Spacing;
use cosmic::iced::core::gradient::Linear;
use cosmic::iced::core::widget::Tree;
use cosmic::iced::core::{
    Alignment, Background, Border, ContentFit, Degrees, Layout, Length, Point, Rectangle, Size,
    layout, overlay,
};
use cosmic::iced::{self, window};
use cosmic::widget::{
    self, button, column, divider, dropdown, icon, image, layer_container, row, space, svg, text,
};
use cosmic_bg_config::Source;
use wayland_client::protocol::wl_output::WlOutput;

use crate::app::OutputState;
use crate::fl;
use crate::screenshot::{Choice, Rect, ScreenshotImage, ToplevelImage};

use super::output_selection::OutputSelection;
use super::rectangle_selection::{DragState, RectangleSelection};

pub struct ScreenshotSelection<'a, Msg> {
    id: cosmic::widget::Id,
    pub choice: Choice,
    pub choices: Vec<Choice>,
    pub output_logical_geo: Vec<Rect>,
    pub choice_labels: Vec<Cow<'a, str>>,
    pub bg_element: Element<'a, Msg>,
    pub fg_element: Element<'a, Msg>,
    pub menu_element: Element<'a, Msg>,
}

// children structure depends on current choice
// 1. select window
//   - bg: bg image
//   - grid layout
//     - fg: fg images (windows)
//   - menu
// 2. select output
//   - bg: bg image
//   - rectangle covering hovered output
//     - this should be 2 quads with a hole in the middle, only visible borders
//   - menu
// 3. select rectangle
//  - bg: bg image
//  - rectangle covering selected region
//    - this should include handles for resizing
//    - if it is across multiple outputs, it should be split into multiple rectangles
//  - menu

// for now lets just support selecting the output

// Keep window previews clear of the controls, which are positioned 32 logical pixels above the
// bottom edge. This includes the controls' height and some breathing room above them.
const WINDOW_PICKER_BOTTOM_INSET: f32 = 128.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowGridLayout {
    columns: usize,
    cell_width: f32,
    cell_height: f32,
    spacing: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct WindowGridPlan {
    layout: WindowGridLayout,
    order: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
struct AnchoredGridScore {
    pointer_distance: f32,
    target_area: f32,
    center_distance: f32,
    total_area: f32,
    column_delta: usize,
    index_delta: usize,
    columns: usize,
}

impl AnchoredGridScore {
    fn is_better_than(self, other: Self) -> bool {
        self.pointer_distance
            .total_cmp(&other.pointer_distance)
            // Once the target can cover the pointer, make that window as prominent as possible
            // before optimizing the rest of the grid.
            .then_with(|| other.target_area.total_cmp(&self.target_area))
            .then_with(|| self.center_distance.total_cmp(&other.center_distance))
            .then_with(|| other.total_area.total_cmp(&self.total_area))
            .then_with(|| self.column_delta.cmp(&other.column_delta))
            .then_with(|| self.index_delta.cmp(&other.index_delta))
            .then_with(|| self.columns.cmp(&other.columns))
            .is_lt()
    }
}

fn window_grid_for_columns(
    window_count: usize,
    available: Size,
    spacing: f32,
    columns: usize,
) -> WindowGridLayout {
    let columns = columns.clamp(1, window_count.max(1));
    let rows = window_count.max(1).div_ceil(columns);
    let horizontal_spacing = if columns > 1 {
        ((available.width - columns as f32) / (columns - 1) as f32).max(0.0)
    } else {
        spacing
    };
    let vertical_spacing = if rows > 1 {
        ((available.height - rows as f32) / (rows - 1) as f32).max(0.0)
    } else {
        spacing
    };
    let candidate_spacing = spacing.min(horizontal_spacing).min(vertical_spacing);
    let cell_width = ((available.width - candidate_spacing * columns.saturating_sub(1) as f32)
        / columns as f32)
        .max(1.0);
    let cell_height = ((available.height - candidate_spacing * rows.saturating_sub(1) as f32)
        / rows as f32)
        .max(1.0);

    WindowGridLayout {
        columns,
        cell_width,
        cell_height,
        spacing: candidate_spacing,
    }
}

fn window_grid_visible_area(dimensions: &[(u32, u32)], grid: WindowGridLayout) -> f32 {
    dimensions
        .iter()
        .map(|&(width, height)| {
            let size = window_preview_size((width, height), grid);
            size.width * size.height
        })
        .sum()
}

fn window_grid_layout(
    dimensions: &[(u32, u32)],
    available: Size,
    spacing: f32,
) -> WindowGridLayout {
    if dimensions.is_empty() {
        return WindowGridLayout {
            columns: 1,
            cell_width: available.width.max(1.0),
            cell_height: available.height.max(1.0),
            spacing: 0.0,
        };
    }

    let mut best = WindowGridLayout {
        columns: 1,
        cell_width: available.width.max(1.0),
        cell_height: available.height.max(1.0),
        spacing: 0.0,
    };
    let mut best_visible_area = -1.0_f32;

    for columns in 1..=dimensions.len() {
        let candidate = window_grid_for_columns(dimensions.len(), available, spacing, columns);
        // Prefer the arrangement that makes the previews collectively largest. ScaleDown does
        // not enlarge a preview beyond its captured size, so account for that here as well.
        let visible_area = window_grid_visible_area(dimensions, candidate);

        if visible_area > best_visible_area {
            best_visible_area = visible_area;
            best = candidate;
        }
    }

    best
}

fn window_preview_size((width, height): (u32, u32), grid: WindowGridLayout) -> Size {
    let width = width.max(1) as f32;
    let height = height.max(1) as f32;
    let scale = (grid.cell_width / width)
        .min(grid.cell_height / height)
        .min(1.0);
    Size::new(width * scale, height * scale)
}

fn window_preview_bounds(
    dimensions: &[(u32, u32)],
    order: &[usize],
    available: Size,
    grid: WindowGridLayout,
) -> Vec<Rectangle> {
    let sizes = dimensions
        .iter()
        .copied()
        .map(|dimensions| window_preview_size(dimensions, grid))
        .collect::<Vec<_>>();
    let row_sizes = order
        .chunks(grid.columns)
        .map(|row| {
            let width = row.iter().map(|&i| sizes[i].width).sum::<f32>()
                + grid.spacing * row.len().saturating_sub(1) as f32;
            let height = row.iter().map(|&i| sizes[i].height).fold(0.0_f32, f32::max);
            Size::new(width, height)
        })
        .collect::<Vec<_>>();
    let content_height = row_sizes.iter().map(|size| size.height).sum::<f32>()
        + grid.spacing * row_sizes.len().saturating_sub(1) as f32;
    let mut y = ((available.height - content_height) / 2.0).max(0.0);
    let mut bounds = Vec::with_capacity(order.len());

    for (row, row_size) in order.chunks(grid.columns).zip(row_sizes) {
        let mut x = ((available.width - row_size.width) / 2.0).max(0.0);
        for &i in row {
            let size = sizes[i];
            bounds.push(Rectangle::new(
                Point::new(x, y + (row_size.height - size.height) / 2.0),
                size,
            ));
            x += size.width + grid.spacing;
        }
        y += row_size.height + grid.spacing;
    }

    bounds
}

fn squared_distance_to_rectangle(point: Point, bounds: Rectangle) -> f32 {
    let dx = if point.x < bounds.x {
        bounds.x - point.x
    } else if point.x > bounds.x + bounds.width {
        point.x - bounds.x - bounds.width
    } else {
        0.0
    };
    let dy = if point.y < bounds.y {
        bounds.y - point.y
    } else if point.y > bounds.y + bounds.height {
        point.y - bounds.y - bounds.height
    } else {
        0.0
    };
    dx * dx + dy * dy
}

fn squared_distance_to_center(point: Point, bounds: Rectangle) -> f32 {
    let dx = point.x - bounds.center_x();
    let dy = point.y - bounds.center_y();
    dx * dx + dy * dy
}

fn anchored_window_grid_plan(
    dimensions: &[(u32, u32)],
    available: Size,
    spacing: f32,
    pointer: Point,
    hovered_index: usize,
) -> WindowGridPlan {
    let original = (0..dimensions.len()).collect::<Vec<_>>();
    if hovered_index >= original.len() {
        return WindowGridPlan {
            layout: window_grid_layout(dimensions, available, spacing),
            order: original,
        };
    }

    let default_columns = window_grid_layout(dimensions, available, spacing).columns;
    let mut best: Option<(WindowGridPlan, AnchoredGridScore)> = None;

    for columns in 1..=dimensions.len() {
        let layout = window_grid_for_columns(dimensions.len(), available, spacing, columns);
        let total_area = window_grid_visible_area(dimensions, layout);

        for slot in 0..original.len() {
            let mut order = original.clone();
            order.swap(hovered_index, slot);
            let bounds = window_preview_bounds(dimensions, &order, available, layout)[slot];
            let score = AnchoredGridScore {
                pointer_distance: squared_distance_to_rectangle(pointer, bounds),
                target_area: bounds.width * bounds.height,
                center_distance: squared_distance_to_center(pointer, bounds),
                total_area,
                column_delta: columns.abs_diff(default_columns),
                index_delta: hovered_index.abs_diff(slot),
                columns,
            };
            let is_better = best
                .as_ref()
                .is_none_or(|(_, best_score)| score.is_better_than(*best_score));
            if is_better {
                best = Some((WindowGridPlan { layout, order }, score));
            }
        }
    }

    best.unwrap().0
}

fn window_at_position(images: &[ToplevelImage], pointer: Point) -> Option<usize> {
    images
        .iter()
        .enumerate()
        .filter(|(_, image)| {
            image.geometry.is_some_and(|geometry| {
                pointer.x >= geometry.left as f32
                    && pointer.x < geometry.right as f32
                    && pointer.y >= geometry.top as f32
                    && pointer.y < geometry.bottom as f32
            })
        })
        // The active window is normally topmost. The protocol has no explicit stacking order,
        // so prefer newer entries when multiple inactive windows overlap.
        .max_by_key(|(i, image)| (image.activated, *i))
        .map(|(i, _)| i)
}

fn toplevel_preview_dimensions(image: &ToplevelImage) -> (u32, u32) {
    image.preview_dimensions
}

impl<'a, Msg> ScreenshotSelection<'a, Msg>
where
    Msg: 'static + Clone,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        choice: Choice,
        image: &ScreenshotImage,
        on_capture: Msg,
        on_cancel: Msg,
        output: &OutputState,
        window_id: window::Id,
        on_output_change: impl Fn(WlOutput) -> Msg,
        on_choice_change: impl Fn(Choice) -> Msg + 'static + Clone,
        toplevel_images: &HashMap<String, Vec<ToplevelImage>>,
        toplevel_chosen: impl Fn(String, usize) -> Msg,
        save_locations: &'a Vec<String>,
        selected_save_location: usize,
        dropdown_selected: impl Fn(usize) -> Msg + 'static + Clone,
        spacing: Spacing,
        dnd_id: u128,
    ) -> Self {
        let space_l = spacing.space_l;
        let space_s = spacing.space_s;
        let space_xs = spacing.space_xs;
        let space_xxs = spacing.space_xxs;

        let output_rect = Rect {
            left: output.logical_pos.0,
            top: output.logical_pos.1,
            right: output.logical_pos.0 + output.logical_size.0 as i32,
            bottom: output.logical_pos.1 + output.logical_size.1 as i32,
        };

        let on_choice_change_clone = on_choice_change.clone();
        let fg_element = match choice {
            Choice::Rectangle(r, drag_state) => RectangleSelection::new(
                output_rect,
                r,
                drag_state,
                window_id,
                dnd_id,
                move |s, r| on_choice_change_clone(Choice::Rectangle(r, s)),
            )
            .into(),
            Choice::Output(_) => {
                OutputSelection::new(on_output_change(output.output.clone()), on_capture.clone())
                    .into()
            }
            Choice::Window(..) => {
                let imgs = toplevel_images
                    .get(&output.name)
                    .map(|x| x.as_slice())
                    .unwrap_or_default();
                let space_l = f32::from(space_l);
                let available = Size::new(
                    (output.logical_size.0 as f32 - 2.0 * space_l).max(1.0),
                    (output.logical_size.1 as f32 - space_l - WINDOW_PICKER_BOTTOM_INSET).max(1.0),
                );
                let dimensions = imgs
                    .iter()
                    .map(toplevel_preview_dimensions)
                    .collect::<Vec<_>>();
                let plan = output
                    .window_pointer_anchor
                    .and_then(|pointer| {
                        window_at_position(imgs, pointer).map(|hovered_index| {
                            anchored_window_grid_plan(
                                &dimensions,
                                available,
                                space_l,
                                Point::new(pointer.x - space_l, pointer.y - space_l),
                                hovered_index,
                            )
                        })
                    })
                    .unwrap_or_else(|| WindowGridPlan {
                        layout: window_grid_layout(&dimensions, available, space_l),
                        order: (0..imgs.len()).collect(),
                    });
                let grid = plan.layout;
                let order = plan.order;

                let img_rows = order.chunks(grid.columns).map(|row| {
                    let img_buttons = row.iter().map(|&i| {
                        let img = &imgs[i].image;
                        let size = window_preview_size(dimensions[i], grid);

                        button::custom(
                            image::Image::new(img.handle.clone())
                                .width(Length::Fill)
                                .height(Length::Fill)
                                .content_fit(ContentFit::ScaleDown),
                        )
                        .width(Length::Fixed(size.width))
                        .height(Length::Fixed(size.height))
                        .padding(0)
                        .on_press(toplevel_chosen(output.name.clone(), i))
                        .class(cosmic::theme::Button::Image)
                        .into()
                    });

                    row::with_children(img_buttons)
                        .spacing(grid.spacing)
                        .align_y(Alignment::Center)
                        .into()
                });

                layer_container(
                    column::with_children(img_rows)
                        .spacing(grid.spacing)
                        .align_x(Alignment::Center),
                )
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .padding([space_l, space_l, WINDOW_PICKER_BOTTOM_INSET, space_l])
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
            }
        };

        let bg_element = match choice {
            Choice::Output(_) | Choice::Rectangle(..) => image::Image::new(image.handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            Choice::Window(..) => match output.bg_source.clone() {
                Some(Source::Path(path)) => image::Image::new(image::Handle::from_path(path))
                    .content_fit(ContentFit::Cover)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                Some(Source::Color(color)) => {
                    layer_container(space::horizontal().width(Length::Fill))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .class(cosmic::theme::Container::Custom(Box::new(move |_| {
                            let color = color.clone();
                            widget::container::Style {
                                background: Some(match color {
                                    cosmic_bg_config::Color::Single(c) => Background::Color(
                                        cosmic::iced::Color::from_rgba(c[0], c[1], c[2], 1.0),
                                    ),
                                    cosmic_bg_config::Color::Gradient(
                                        cosmic_bg_config::Gradient { colors, radius },
                                    ) => {
                                        let stop_increment = 1.0 / (colors.len() - 1) as f32;
                                        let mut stop = 0.0;

                                        let mut linear = Linear::new(Degrees(radius));

                                        for &[r, g, b] in colors.iter() {
                                            linear = linear.add_stop(
                                                stop,
                                                cosmic::iced::Color::from_rgb(r, g, b),
                                            );
                                            stop += stop_increment;
                                        }

                                        Background::Gradient(cosmic::iced::core::Gradient::Linear(
                                            linear,
                                        ))
                                    }
                                }),
                                ..Default::default()
                            }
                        })))
                        .into()
                }
                None => image::Image::new(image::Handle::from_path(
                    "/usr/share/backgrounds/cosmic/orion_nebula_nasa_heic0601a.jpg",
                ))
                .content_fit(ContentFit::Cover)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            },
        };
        let active_icon = cosmic::theme::Svg::Custom(Rc::new(|t| svg::Style {
            color: Some(t.cosmic().accent_color().into()),
        }));
        Self {
            id: cosmic::widget::Id::unique(),
            choices: Vec::new(),
            output_logical_geo: Vec::new(),
            choice_labels: Vec::new(),
            bg_element,
            fg_element,
            menu_element: cosmic::widget::container(
                row![
                    row![
                        button::custom(
                            icon::Icon::from(
                                icon::from_name("screenshot-selection-symbolic").size(64)
                            )
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                            .class(
                                if matches!(choice, Choice::Rectangle(..)) {
                                    active_icon.clone()
                                } else {
                                    cosmic::theme::Svg::default()
                                }
                            )
                        )
                        .selected(matches!(choice, Choice::Rectangle(..)))
                        .class(cosmic::theme::Button::Icon)
                        .on_press(on_choice_change(Choice::Rectangle(
                            Rect::default(),
                            DragState::None
                        )))
                        .padding(space_xs),
                        button::custom(
                            icon::Icon::from(
                                icon::from_name("screenshot-window-symbolic").size(64)
                            )
                            .class(if matches!(choice, Choice::Window(..)) {
                                active_icon.clone()
                            } else {
                                cosmic::theme::Svg::default()
                            })
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                        )
                        .selected(matches!(choice, Choice::Window(..)))
                        .class(cosmic::theme::Button::Icon)
                        .on_press(on_choice_change(Choice::Window(output.name.clone(), None)))
                        .padding(space_xs),
                        button::custom(
                            icon::Icon::from(
                                icon::from_name("screenshot-screen-symbolic").size(64)
                            )
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                            .class(
                                if matches!(choice, Choice::Output(..)) {
                                    active_icon.clone()
                                } else {
                                    cosmic::theme::Svg::default()
                                }
                            )
                        )
                        .selected(matches!(choice, Choice::Output(..)))
                        .class(cosmic::theme::Button::Icon)
                        .on_press(on_choice_change(Choice::Output(output.name.clone())))
                        .padding(space_xs)
                    ]
                    .spacing(space_s)
                    .align_y(Alignment::Center),
                    divider::vertical::light().height(Length::Fixed(64.0)),
                    button::custom(text(fl!("capture"))).on_press_maybe(
                        if let Choice::Rectangle(r, ..) = choice {
                            // Disable button on empty selection
                            r.dimensions().is_some().then_some(on_capture)
                        } else {
                            Some(on_capture)
                        }
                    ),
                    divider::vertical::light().height(Length::Fixed(64.0)),
                    Element::from(dropdown(
                        save_locations.as_slice(),
                        Some(selected_save_location),
                        |i| i
                    ))
                    .map(dropdown_selected),
                    divider::vertical::light().height(Length::Fixed(64.0)),
                    button::custom(
                        icon::Icon::from(icon::from_name("window-close-symbolic").size(63))
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                    )
                    .class(cosmic::theme::Button::Icon)
                    .on_press(on_cancel),
                ]
                .align_y(Alignment::Center)
                .spacing(space_s)
                .padding([space_xxs, space_s, space_xxs, space_s]),
            )
            .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                let cosmic = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(
                        // TODO support blur effect in iced?
                        cosmic.background(false).component.base.into(),
                    )),
                    text_color: Some(cosmic.background(false).component.on.into()),
                    border: Border {
                        radius: cosmic.corner_radii.radius_s.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })))
            .into(),
            choice,
        }
    }
}

impl<'a, Msg> cosmic::widget::Widget<Msg, cosmic::Theme, cosmic::Renderer>
    for ScreenshotSelection<'a, Msg>
{
    fn children(&self) -> Vec<cosmic::iced::core::widget::Tree> {
        vec![
            Tree::new(&self.bg_element),
            Tree::new(&self.fg_element),
            Tree::new(&self.menu_element),
        ]
    }

    fn diff(&mut self, tree: &mut cosmic::iced::core::widget::Tree) {
        tree.diff_children(&mut [
            &mut self.bg_element,
            &mut self.fg_element,
            &mut self.menu_element,
        ]);
    }

    fn overlay<'b>(
        &'b mut self,
        state: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &cosmic::iced::core::Rectangle,
        translation: iced::Vector,
    ) -> Option<overlay::Element<'b, Msg, cosmic::Theme, cosmic::Renderer>> {
        let children = [
            &mut self.bg_element,
            &mut self.fg_element,
            &mut self.menu_element,
        ]
        .into_iter()
        .zip(&mut state.children)
        .zip(layout.children())
        .filter_map(|((child, state), layout)| {
            child
                .as_widget_mut()
                .overlay(state, layout, renderer, viewport, translation)
        })
        .collect::<Vec<_>>();

        (!children.is_empty()).then(|| overlay::Group::with_children(children).overlay())
    }

    fn update(
        &mut self,
        tree: &mut cosmic::iced::core::widget::Tree,
        event: &cosmic::iced::core::Event,
        layout: Layout<'_>,
        cursor: cosmic::iced::core::mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn cosmic::iced::core::Clipboard,
        shell: &mut cosmic::iced::core::Shell<'_, Msg>,
        viewport: &cosmic::iced::core::Rectangle,
    ) {
        let children = [
            &mut self.bg_element,
            &mut self.fg_element,
            &mut self.menu_element,
        ];

        let layout = layout.children().collect::<Vec<_>>();
        // draw children in order
        for (i, (layout, child)) in layout.into_iter().zip(children).enumerate().rev() {
            let tree = &mut tree.children[i];

            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
            if shell.is_event_captured() {
                break;
            }
        }
    }

    fn mouse_interaction(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        cursor: cosmic::iced::core::mouse::Cursor,
        viewport: &cosmic::iced::core::Rectangle,
        renderer: &cosmic::Renderer,
    ) -> cosmic::iced::core::mouse::Interaction {
        let children = [&self.bg_element, &self.fg_element, &self.menu_element];
        let layout = layout.children().collect::<Vec<_>>();
        for (i, (layout, child)) in layout.into_iter().zip(children).enumerate().rev() {
            let tree = &state.children[i];
            let interaction = child
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer);
            if cursor.is_over(layout.bounds()) {
                return interaction;
            }
        }
        cosmic::iced::core::mouse::Interaction::default()
    }

    fn operate(
        &mut self,
        tree: &mut cosmic::iced::core::widget::Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn cosmic::widget::Operation<()>,
    ) {
        let layout = layout.children().collect::<Vec<_>>();
        let children = [
            &mut self.bg_element,
            &mut self.fg_element,
            &mut self.menu_element,
        ];
        for (i, (layout, child)) in layout.into_iter().zip(children).enumerate().rev() {
            let tree = &mut tree.children[i];
            child
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
        }
    }

    fn id(&self) -> Option<cosmic::widget::Id> {
        Some(self.id.clone())
    }

    fn set_id(&mut self, _id: cosmic::widget::Id) {
        self.id = _id;
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut cosmic::iced::core::widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &cosmic::iced::core::layout::Limits,
    ) -> cosmic::iced::core::layout::Node {
        let children = &mut tree.children;
        let bg_image = &mut children[0];
        let bg_node = self
            .bg_element
            .as_widget_mut()
            .layout(bg_image, renderer, limits);
        let fg_node = self
            .fg_element
            .as_widget_mut()
            .layout(&mut children[1], renderer, limits);
        let mut menu_node =
            self.menu_element
                .as_widget_mut()
                .layout(&mut children[2], renderer, limits);
        let menu_bounds = menu_node.bounds();
        menu_node = menu_node.move_to(Point {
            x: (limits.max().width - menu_bounds.width) / 2.0,
            y: limits.max().height - menu_bounds.height - 32.0,
        });

        layout::Node::with_children(
            limits.resolve(Length::Fill, Length::Fill, Size::ZERO),
            vec![bg_node, fg_node, menu_node],
        )
    }

    fn draw(
        &self,
        tree: &cosmic::iced::core::widget::Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &cosmic::iced::core::renderer::Style,
        layout: cosmic::iced::core::Layout<'_>,
        cursor: cosmic::iced::core::mouse::Cursor,
        viewport: &cosmic::iced::core::Rectangle,
    ) {
        use cosmic::iced::core::Renderer;
        let children = &[&self.bg_element, &self.fg_element, &self.menu_element];
        let mut children = layout.children().zip(children).enumerate();
        {
            let (i, (layout, child)) = children.next().unwrap();
            let bg_tree = &tree.children[i];
            child
                .as_widget()
                .draw(bg_tree, renderer, theme, style, layout, cursor, viewport);
        }

        // draw children in order
        for (i, (layout, child)) in children {
            renderer.with_layer(layout.bounds(), |renderer| {
                let tree = &tree.children[i];
                child
                    .as_widget()
                    .draw(tree, renderer, theme, style, layout, cursor, viewport);
            });
        }
    }

    fn drag_destinations(
        &self,
        state: &cosmic::iced::core::widget::Tree,
        layout: cosmic::iced::core::Layout<'_>,
        renderer: &cosmic::Renderer,
        dnd_rectangles: &mut cosmic::iced::core::clipboard::DndDestinationRectangles,
    ) {
        let children = &[&self.bg_element, &self.fg_element, &self.menu_element];
        for (i, (layout, child)) in layout.children().zip(children).enumerate() {
            let state = &state.children[i];
            child
                .as_widget()
                .drag_destinations(state, layout, renderer, dnd_rectangles);
        }
    }
}

impl<'a, Message> From<ScreenshotSelection<'a, Message>> for cosmic::Element<'a, Message>
where
    Message: 'static + Clone,
{
    fn from(w: ScreenshotSelection<'a, Message>) -> cosmic::Element<'a, Message> {
        Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_grid_uses_multiple_rows_for_many_landscape_windows() {
        let dimensions = vec![(1600, 900); 8];
        let layout = window_grid_layout(&dimensions, Size::new(1872.0, 876.0), 24.0);

        assert_eq!(layout.columns, 3);
        assert_eq!(dimensions.len().div_ceil(layout.columns), 3);
        assert_eq!(layout.cell_width, 608.0);
        assert_eq!(layout.cell_height, 276.0);
    }

    #[test]
    fn window_grid_keeps_every_cell_inside_the_available_area() {
        let dimensions = [
            (1920, 1080),
            (800, 1200),
            (1000, 700),
            (600, 900),
            (1600, 900),
            (500, 500),
        ];
        let available = Size::new(1872.0, 876.0);
        let spacing = 24.0;
        let layout = window_grid_layout(&dimensions, available, spacing);
        let rows = dimensions.len().div_ceil(layout.columns);

        let used_width = layout.cell_width * layout.columns as f32
            + layout.spacing * layout.columns.saturating_sub(1) as f32;
        let used_height =
            layout.cell_height * rows as f32 + layout.spacing * rows.saturating_sub(1) as f32;

        assert!(used_width <= available.width);
        assert!(used_height <= available.height);
        assert!(layout.columns < dimensions.len());
    }

    #[test]
    fn window_grid_places_hovered_window_under_pointer() {
        let dimensions = vec![(1600, 900); 4];
        let available = Size::new(1000.0, 800.0);
        let pointer = Point::new(800.0, 600.0);
        let plan = anchored_window_grid_plan(&dimensions, available, 20.0, pointer, 0);
        let target_slot = plan.order.iter().position(|&i| i == 0).unwrap();
        let bounds = window_preview_bounds(&dimensions, &plan.order, available, plan.layout);

        assert_ne!(target_slot, 0);
        assert!(bounds[target_slot].contains(pointer));
    }

    #[test]
    fn hovered_window_influences_grid_shape() {
        let dimensions = vec![(1600, 200), (400, 1000), (400, 1000), (400, 1000)];
        let available = Size::new(1000.0, 800.0);
        let pointer = Point::new(500.0, 100.0);
        let default = window_grid_layout(&dimensions, available, 20.0);
        let plan = anchored_window_grid_plan(&dimensions, available, 20.0, pointer, 0);
        let target_slot = plan.order.iter().position(|&i| i == 0).unwrap();
        let bounds = window_preview_bounds(&dimensions, &plan.order, available, plan.layout);

        assert_eq!(default.columns, 4);
        assert_eq!(plan.layout.columns, 1);
        assert!(bounds[target_slot].contains(pointer));
        assert!(
            bounds[target_slot].width * bounds[target_slot].height
                > window_preview_size(dimensions[0], default).width
                    * window_preview_size(dimensions[0], default).height
        );
    }

    #[test]
    fn active_window_wins_when_geometries_overlap() {
        let image = || ToplevelImage {
            image: ScreenshotImage {
                rgba: ::image::RgbaImage::new(1, 1),
                handle: cosmic::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
            },
            geometry: Some(Rect {
                left: 0,
                top: 0,
                right: 500,
                bottom: 500,
            }),
            preview_dimensions: (500, 500),
            activated: false,
        };
        let mut images = vec![image(), image()];
        images[1].activated = true;

        assert_eq!(
            window_at_position(&images, Point::new(250.0, 250.0)),
            Some(1)
        );
    }
}
