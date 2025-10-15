use std::{borrow::Cow, collections::HashMap, rc::Rc};

use cosmic::{
    Element,
    cosmic_theme::Spacing,
    iced::{self, window},
    iced_core::{
        Background, Border, ContentFit, Degrees, Layout, Length, Point, Size, alignment,
        gradient::Linear, layout, overlay, widget::Tree,
    },
    iced_widget::row,
    iced_winit::platform_specific::wayland::subsurface_widget::Subsurface,
    widget::{
        Row, button, divider::vertical, dropdown, horizontal_space, icon, image, layer_container,
        text,
    },
};
use cosmic_bg_config::Source;
use wayland_client::protocol::wl_output::WlOutput;

use crate::{
    app::OutputState,
    fl,
    screenshot::{Choice, Rect, ScreenshotImage},
};

use super::{
    output_selection::OutputSelection,
    rectangle_selection::{DragState, RectangleSelection},
};

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

impl<'a, Msg> ScreenshotSelection<'a, Msg>
where
    Msg: 'static + Clone,
{
    pub fn new(
        choice: Choice,
        image: &ScreenshotImage,
        on_capture: Msg,
        on_cancel: Msg,
        output: &OutputState,
        window_id: window::Id,
        on_output_change: impl Fn(WlOutput) -> Msg,
        on_choice_change: impl Fn(Choice) -> Msg + 'static + Clone,
        toplevel_images: &HashMap<String, Vec<ScreenshotImage>>,
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
                let total_img_width = imgs.iter().map(|img| img.width()).sum::<u32>();

                let img_buttons = imgs.iter().enumerate().map(|(i, img)| {
                    let portion =
                        (img.width() as u64 * u16::MAX as u64 / total_img_width as u64).max(1);
                    layer_container(
                        button::custom(
                            Subsurface::new(img.subsurface_buffer.clone())
                                //.z(1)
                                .z(-1)
                                .transform(img.transform.clone())
                                .content_fit(ContentFit::ScaleDown),
                        )
                        .on_press(toplevel_chosen(output.name.clone(), i))
                        .class(cosmic::theme::Button::Image),
                    )
                    .align_x(alignment::Alignment::Center)
                    .width(Length::FillPortion(portion as u16))
                    .height(Length::Shrink)
                    .into()
                });
                layer_container(
                    Row::with_children(img_buttons)
                        .spacing(space_l)
                        .width(Length::Fill)
                        .align_y(alignment::Alignment::Center)
                        .padding(space_l),
                )
                .align_x(alignment::Alignment::Center)
                .align_y(alignment::Alignment::Center)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
            }
        };

        let fd = crate::buffer::create_memfd(1, 1);
        rustix::io::write(&fd, &[255, 0, 0, 255]).unwrap();
        let shmbuf = cosmic::iced_winit::platform_specific::wayland::subsurface_widget::Shmbuf {
            fd,
            height: 1,
            width: 1,
            offset: 0,
            stride: 4,
            format: wayland_client::protocol::wl_shm::Format::Abgr8888,
        };
        let (subsurface_buffer, _) =
            cosmic::iced_winit::platform_specific::wayland::subsurface_widget::SubsurfaceBuffer::new(std::sync::Arc::new(shmbuf.into()));
        let bg_element = Subsurface::new(subsurface_buffer)
            //.z(2)
            .z(-2)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();

        /*
        let bg_element = match choice {
            Choice::Output(_) | Choice::Rectangle(..) => {
                Subsurface::new(image.subsurface_buffer.clone())
                    .transform(image.transform.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            }
            Choice::Window(..) => match output.bg_source.clone() {
                Some(Source::Path(path)) => image::Image::new(image::Handle::from_path(path))
                    .content_fit(ContentFit::Cover)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                Some(Source::Color(color)) => {
                    layer_container(horizontal_space().width(Length::Fill))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .class(cosmic::theme::Container::Custom(Box::new(move |_| {
                            let color = color.clone();
                            cosmic::iced_widget::container::Style {
                                background: Some(match color {
                                    cosmic_bg_config::Color::Single(c) => Background::Color(
                                        cosmic::iced::Color::new(c[0], c[1], c[2], 1.0),
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

                                        Background::Gradient(cosmic::iced_core::Gradient::Linear(
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
                    "/usr/share/backgrounds/pop/kate-hazen-COSMIC-desktop-wallpaper.png",
                ))
                .content_fit(ContentFit::Cover)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            },
        };
        */
        let active_icon =
            cosmic::theme::Svg::Custom(Rc::new(|t| cosmic::iced_widget::svg::Style {
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
                    .align_y(cosmic::iced_core::Alignment::Center),
                    vertical::light().height(Length::Fixed(64.0)),
                    button::custom(text(fl!("capture"))).on_press_maybe(
                        if let Choice::Rectangle(r, ..) = choice {
                            // Disable button on empty selection
                            r.dimensions().is_some().then_some(on_capture)
                        } else {
                            Some(on_capture)
                        }
                    ),
                    vertical::light().height(Length::Fixed(64.0)),
                    Element::from(dropdown(
                        save_locations.as_slice(),
                        Some(selected_save_location),
                        |i| i
                    ))
                    .map(dropdown_selected),
                    vertical::light().height(Length::Fixed(64.0)),
                    button::custom(
                        icon::Icon::from(icon::from_name("window-close-symbolic").size(63))
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                    )
                    .class(cosmic::theme::Button::Icon)
                    .on_press(on_cancel),
                ]
                .align_y(cosmic::iced_core::Alignment::Center)
                .spacing(space_s)
                .padding([space_xxs, space_s, space_xxs, space_s]),
            )
            .class(cosmic::theme::Container::Custom(Box::new(|theme| {
                let theme = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(theme.background.component.base.into())),
                    text_color: Some(theme.background.component.on.into()),
                    border: Border {
                        radius: theme.corner_radii.radius_s.into(),
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
    fn children(&self) -> Vec<cosmic::iced_core::widget::Tree> {
        vec![
            Tree::new(&self.bg_element),
            Tree::new(&self.fg_element),
            Tree::new(&self.menu_element),
        ]
    }

    fn diff(&mut self, tree: &mut cosmic::iced_core::widget::Tree) {
        tree.diff_children(&mut [
            &mut self.bg_element,
            &mut self.fg_element,
            &mut self.menu_element,
        ]);
    }

    fn overlay<'b>(
        &'b mut self,
        state: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        translation: iced::Vector,
    ) -> Option<cosmic::iced_core::overlay::Element<'b, Msg, cosmic::Theme, cosmic::Renderer>> {
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
                .overlay(state, layout, renderer, translation)
        })
        .collect::<Vec<_>>();

        (!children.is_empty()).then(|| overlay::Group::with_children(children).overlay())
    }

    fn on_event(
        &mut self,
        tree: &mut cosmic::iced_core::widget::Tree,
        event: cosmic::iced_core::Event,
        layout: Layout<'_>,
        cursor: cosmic::iced_core::mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn cosmic::iced_core::Clipboard,
        shell: &mut cosmic::iced_core::Shell<'_, Msg>,
        viewport: &cosmic::iced_core::Rectangle,
    ) -> cosmic::iced_core::event::Status {
        let children = [
            &mut self.bg_element,
            &mut self.fg_element,
            &mut self.menu_element,
        ];

        let layout = layout.children().collect::<Vec<_>>();
        // draw children in order
        let mut status = cosmic::iced_core::event::Status::Ignored;
        for (i, (layout, child)) in layout
            .into_iter()
            .zip(children.into_iter())
            .enumerate()
            .rev()
        {
            let tree = &mut tree.children[i];

            status = child.as_widget_mut().on_event(
                tree,
                event.clone(),
                layout,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
            if matches!(event, cosmic::iced_core::event::Event::PlatformSpecific(_)) {
                continue;
            }
            if matches!(status, cosmic::iced_core::event::Status::Captured) {
                return status;
            }
        }
        status
    }

    fn mouse_interaction(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        cursor: cosmic::iced_core::mouse::Cursor,
        viewport: &cosmic::iced_core::Rectangle,
        renderer: &cosmic::Renderer,
    ) -> cosmic::iced_core::mouse::Interaction {
        let children = [&self.bg_element, &self.fg_element, &self.menu_element];
        let layout = layout.children().collect::<Vec<_>>();
        for (i, (layout, child)) in layout
            .into_iter()
            .zip(children.into_iter())
            .enumerate()
            .rev()
        {
            let tree = &state.children[i];
            let interaction = child
                .as_widget()
                .mouse_interaction(tree, layout, cursor, viewport, renderer);
            if cursor.is_over(layout.bounds()) {
                return interaction;
            }
        }
        cosmic::iced_core::mouse::Interaction::default()
    }

    fn operate(
        &self,
        tree: &mut cosmic::iced_core::widget::Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn cosmic::widget::Operation<()>,
    ) {
        let layout = layout.children().collect::<Vec<_>>();
        let children = [&self.bg_element, &self.fg_element, &self.menu_element];
        for (i, (layout, child)) in layout
            .into_iter()
            .zip(children.into_iter())
            .enumerate()
            .rev()
        {
            let tree = &mut tree.children[i];
            child.as_widget().operate(tree, layout, renderer, operation);
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
        &self,
        tree: &mut cosmic::iced_core::widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &cosmic::iced_core::layout::Limits,
    ) -> cosmic::iced_core::layout::Node {
        let children = &mut tree.children;
        let bg_image = &mut children[0];
        let bg_node = self
            .bg_element
            .as_widget()
            .layout(bg_image, renderer, limits);
        let fg_node = self
            .fg_element
            .as_widget()
            .layout(&mut children[1], renderer, limits);
        let mut menu_node =
            self.menu_element
                .as_widget()
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
        tree: &cosmic::iced_core::widget::Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &cosmic::iced_core::renderer::Style,
        layout: cosmic::iced_core::Layout<'_>,
        cursor: cosmic::iced_core::mouse::Cursor,
        viewport: &cosmic::iced_core::Rectangle,
    ) {
        use cosmic::iced_core::Renderer;
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
        state: &cosmic::iced_core::widget::Tree,
        layout: cosmic::iced_core::Layout<'_>,
        renderer: &cosmic::Renderer,
        dnd_rectangles: &mut cosmic::iced_core::clipboard::DndDestinationRectangles,
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
