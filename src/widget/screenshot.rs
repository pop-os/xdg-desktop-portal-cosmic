use std::{borrow::Cow, sync::Arc};

use ::image::{EncodableLayout, RgbaImage};
use cosmic::{
    iced::window,
    iced_core::{layout, widget::Tree, Layout, Length, Point, Size},
    iced_widget::row,
    widget::{button, divider::vertical, icon, image, text},
    Element,
};
use wayland_client::protocol::wl_output::WlOutput;

use crate::{
    fl,
    screenshot::{Choice, DndCommand, Rect},
};

use super::{
    output_selection::OutputSelection,
    rectangle_selection::{DragState, RectangleSelection},
};

// TODO: place window images in a Row dense layout

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

pub struct MyImage(Arc<RgbaImage>);

impl AsRef<[u8]> for MyImage {
    fn as_ref(&self) -> &[u8] {
        &self.0.as_bytes()
    }
}

impl<'a, Msg> ScreenshotSelection<'a, Msg>
where
    Msg: 'static + Clone,
{
    pub fn new(
        choice: Choice,
        raw_image: Arc<RgbaImage>,
        on_capture: Msg,
        on_cancel: Msg,
        output: (WlOutput, Rect, String),
        window_id: window::Id,
        on_output_change: impl Fn(WlOutput) -> Msg,
        on_choice_change: impl Fn(Choice) -> Msg + 'static + Clone,
        on_drag_cmd_produced: impl Fn(DndCommand) -> Msg + 'static,
    ) -> Self {
        let space_s = 8.0;
        let space_xs = 4.0;
        let space_xxs = 2.0;

        let on_choice_change_clone = on_choice_change.clone();
        let fg_element = match choice {
            Choice::Rectangle(r, drag_state) => RectangleSelection::new(
                output.1,
                r,
                drag_state,
                window_id,
                move |s, r| on_choice_change_clone(Choice::Rectangle(r, s)),
                on_drag_cmd_produced,
            )
            .into(),
            Choice::Output(_) => {
                OutputSelection::new(on_output_change(output.0), on_capture.clone()).into()
            }
            Choice::Window(_) => todo!(),
        };
        Self {
            id: cosmic::widget::Id::unique(),
            choice,
            choices: Vec::new(),
            output_logical_geo: Vec::new(),
            choice_labels: Vec::new(),
            bg_element: image::Image::new(image::Handle::from_pixels(
                raw_image.width(),
                raw_image.height(),
                MyImage(raw_image),
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
            fg_element,
            menu_element: cosmic::widget::container(
                row![
                    row![
                        button(
                            icon::Icon::from(
                                icon::from_name("screenshot-selection-symbolic").size(64)
                            )
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                        )
                        .on_press(on_choice_change(Choice::Rectangle(
                            Rect::default(),
                            DragState::None
                        )))
                        .padding(space_xs),
                        button(
                            icon::Icon::from(
                                icon::from_name("screenshot-window-symbolic").size(64)
                            )
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                        )
                        .padding(space_xs),
                        button(
                            icon::Icon::from(
                                icon::from_name("screenshot-screen-symbolic").size(64)
                            )
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                        )
                        .on_press(on_choice_change(Choice::Output(output.2.clone())))
                        .padding(space_xs)
                    ]
                    .spacing(space_s)
                    .align_items(cosmic::iced_core::Alignment::Center),
                    vertical::light().height(Length::Fixed(64.0)),
                    button(text(fl!("capture"))).on_press(on_capture),
                    vertical::light().height(Length::Fixed(64.0)),
                    button("todo menu"),
                    vertical::light().height(Length::Fixed(64.0)),
                    button(
                        icon::Icon::from(icon::from_name("window-close-symbolic").size(63))
                            .width(Length::Fixed(40.0))
                            .height(Length::Fixed(40.0))
                    )
                    .on_press(on_cancel),
                ]
                .align_items(cosmic::iced_core::Alignment::Center)
                .spacing(space_s)
                .padding([space_xxs, space_s, space_xxs, space_s]),
            )
            .style(cosmic::theme::Container::Background)
            .into(),
        }
    }
}

impl<'a, Msg> cosmic::widget::Widget<Msg, cosmic::Renderer> for ScreenshotSelection<'a, Msg> {
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
        ])
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
        // TODO delegate to children
        // first check if over menu
        // then check if over fg
        // then check if over bg
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
        // TODO delegate to children
        // first check if over menu
        // then check if over fg
        // then check if over bg
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
        operation: &mut dyn cosmic::widget::Operation<
            cosmic::iced_core::widget::OperationOutputWrapper<Msg>,
        >,
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

    fn width(&self) -> Length {
        Length::Fill
    }

    fn height(&self) -> Length {
        Length::Fill
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
        menu_node.move_to(Point {
            x: (limits.max().width - menu_bounds.width) / 2.0,
            y: limits.max().height - menu_bounds.height - 32.0,
        });

        layout::Node::with_children(
            limits.resolve(Size::ZERO),
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
        let children = &[&self.bg_element, &self.fg_element, &self.menu_element];
        // draw children in order
        for (i, (layout, child)) in layout.children().zip(children).enumerate() {
            let tree = &tree.children[i];
            child
                .as_widget()
                .draw(&tree, renderer, theme, style, layout, cursor, viewport);
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
