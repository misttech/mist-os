// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use carnelian::color::Color;
use carnelian::drawing::{DisplayRotation, FontFace};
use carnelian::render::rive::load_rive;
use carnelian::scene::facets::{
    RiveFacet, TextFacetOptions, TextHorizontalAlignment, TextVerticalAlignment,
};
use carnelian::scene::layout::{
    CrossAxisAlignment, Flex, FlexOptions, MainAxisAlignment, MainAxisSize,
};
use carnelian::scene::scene::{Scene, SceneBuilder};
use carnelian::{
    input, App, AppAssistant, AppAssistantPtr, Point, Size, ViewAssistant, ViewAssistantContext,
    ViewAssistantPtr, ViewKey,
};
use euclid::size2;
use fidl_fuchsia_input_report::ConsumerControlButton;
use fuchsia_async as fasync;

mod menu;
use menu::Menu;
mod power;

const LOGO_IMAGE_PATH: &str = "/system-recovery-config/logo.riv";
const BG_COLOR: Color = Color::new(); // Black
const HEADER_COLOR: Color = Color { r: 249, g: 194, b: 0, a: 255 };
const MENU_COLOR: Color = Color { r: 0, g: 106, b: 157, a: 255 };
const MENU_ACTIVE_BG_COLOR: Color = Color { r: 0, g: 156, b: 100, a: 255 };
const MENU_SELECTED_COLOR: Color = Color::white();

struct RecoveryAppAssistant {
    display_rotation: DisplayRotation,
}

impl RecoveryAppAssistant {
    fn new(display_rotation: DisplayRotation) -> Self {
        Self { display_rotation }
    }
}

impl AppAssistant for RecoveryAppAssistant {
    fn setup(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn create_view_assistant(&mut self, view_key: ViewKey) -> Result<ViewAssistantPtr, Error> {
        Ok(Box::new(RecoveryViewAssistant::new(view_key)?))
    }

    fn filter_config(&mut self, config: &mut carnelian::app::Config) {
        config.view_mode = carnelian::app::ViewMode::Direct;
        config.display_rotation = self.display_rotation;
    }
}

struct RecoveryViewAssistant {
    view_key: ViewKey,
    font_face: FontFace,
    logo_file: Option<rive_rs::File>,
    scene: Option<Scene>,
    menu: Menu,
    logs: Option<Vec<String>>,
    wheel_diff: i32,
}

impl RecoveryViewAssistant {
    fn new(view_key: ViewKey) -> Result<RecoveryViewAssistant, Error> {
        let font_face = recovery_ui::font::get_default_font_face().clone();
        let logo_file = load_rive(LOGO_IMAGE_PATH).ok();
        let menu = Menu::new(menu::MAIN_MENU);

        Ok(RecoveryViewAssistant {
            view_key,
            font_face,
            logo_file,
            scene: None,
            menu,
            logs: None,
            wheel_diff: 0,
        })
    }

    fn log(&mut self, log: impl Into<String>) {
        let log = log.into();
        log::info!("log: {log}");
        self.logs.get_or_insert_default().push(log);
    }
}

impl ViewAssistant for RecoveryViewAssistant {
    fn setup(&mut self, context: &ViewAssistantContext) -> Result<(), Error> {
        self.view_key = context.key;
        Ok(())
    }

    fn get_scene(&mut self, size: Size) -> Option<&mut Scene> {
        Some(self.scene.get_or_insert_with(|| {
            let mut builder =
                SceneBuilder::new().background_color(BG_COLOR).round_scene_corners(true);
            builder.group().column().max_size().main_align(MainAxisAlignment::Start).contents(
                |builder| {
                    if let Some(logo_file) = &self.logo_file {
                        // Centre the logo
                        builder.start_group(
                            "logo_row",
                            Flex::with_options_ptr(FlexOptions::row(
                                MainAxisSize::Max,
                                MainAxisAlignment::Center,
                                CrossAxisAlignment::End,
                            )),
                        );

                        let logo_size: Size = size2(50.0, 50.0);
                        let facet = RiveFacet::new_from_file(logo_size, &logo_file, None)
                            .expect("facet_from_file");
                        builder.facet(Box::new(facet));
                        builder.end_group(); // logo_row
                    }

                    builder.space(size2(size.width, 10.0));

                    let text_size = 25.0;
                    builder.text(
                        self.font_face.clone(),
                        "Android Recovery",
                        text_size,
                        Point::zero(),
                        TextFacetOptions {
                            horizontal_alignment: TextHorizontalAlignment::Center,
                            color: HEADER_COLOR,
                            ..TextFacetOptions::default()
                        },
                    );

                    builder.space(size2(size.width, 40.0));

                    builder
                        .group()
                        .column()
                        .max_size()
                        .main_align(MainAxisAlignment::Start)
                        .cross_align(CrossAxisAlignment::Start)
                        .contents(|builder| {
                            if let Some(logs) = &self.logs {
                                builder.text(
                                    self.font_face.clone(),
                                    &logs.join("\n"),
                                    text_size,
                                    Point::zero(),
                                    TextFacetOptions {
                                        color: Color::white(),
                                        horizontal_alignment: TextHorizontalAlignment::Left,
                                        ..TextFacetOptions::default()
                                    },
                                );
                                return;
                            }

                            const MENU_ITEM_HEIGHT: f32 = 30.0;

                            for item in self.menu.items() {
                                builder.group().stack().contents(|builder| {
                                    builder
                                        .group()
                                        .row()
                                        .max_size()
                                        .cross_align(CrossAxisAlignment::Start)
                                        .contents(|builder| {
                                            // padding on the left of the menu text
                                            builder
                                                .space(size2(size.width * 0.1, MENU_ITEM_HEIGHT));
                                            builder.text(
                                                self.font_face.clone(),
                                                item.title(),
                                                text_size,
                                                Point::zero(),
                                                TextFacetOptions {
                                                    horizontal_alignment:
                                                        TextHorizontalAlignment::Left,
                                                    vertical_alignment:
                                                        TextVerticalAlignment::Center,
                                                    color: if self.menu.current_item() == item {
                                                        MENU_SELECTED_COLOR
                                                    } else {
                                                        MENU_COLOR
                                                    },
                                                    max_width: Some(size.width * 0.8),
                                                    ..TextFacetOptions::default()
                                                },
                                            );
                                        });

                                    let rect_size = size2(size.width, MENU_ITEM_HEIGHT);
                                    if self.menu.current_item() == item {
                                        builder.rectangle(
                                            rect_size,
                                            if self.menu.is_active() {
                                                MENU_ACTIVE_BG_COLOR
                                            } else {
                                                MENU_COLOR
                                            },
                                        );
                                    } else {
                                        builder.space(rect_size);
                                    }
                                });
                            }
                        });
                },
            );

            builder.build()
        }))
    }

    fn handle_mouse_event(
        &mut self,
        context: &mut ViewAssistantContext,
        _event: &input::Event,
        mouse_event: &input::mouse::Event,
    ) -> Result<(), Error> {
        if self.logs.is_some() {
            return Ok(());
        }
        match mouse_event.phase {
            input::mouse::Phase::Wheel(vector) => {
                self.wheel_diff += vector.y;
                if self.wheel_diff > 80 {
                    self.wheel_diff = 0;
                    self.menu.move_up();
                    self.scene = None;
                    context.request_render();
                } else if self.wheel_diff < -80 {
                    self.wheel_diff = 0;
                    self.menu.move_down();
                    self.scene = None;
                    context.request_render();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_consumer_control_event(
        &mut self,
        context: &mut ViewAssistantContext,
        _event: &input::Event,
        consumer_control_event: &input::consumer_control::Event,
    ) -> Result<(), Error> {
        if self.logs.is_some() {
            return Ok(());
        }
        match consumer_control_event {
            input::consumer_control::Event {
                button: ConsumerControlButton::Power,
                phase: input::consumer_control::Phase::Down,
            } => {
                self.menu.set_active(true);
            }
            input::consumer_control::Event {
                button: ConsumerControlButton::Function,
                phase: input::consumer_control::Phase::Up,
            } => {
                self.menu.move_down();
            }
            input::consumer_control::Event {
                button: ConsumerControlButton::Power,
                phase: input::consumer_control::Phase::Up,
            } => match self.menu.current_item() {
                menu::MenuItem::Reboot => {
                    self.log("Rebooting...");
                    fasync::Task::local(async {
                        // give UI some time to show the log above
                        fasync::Timer::new(std::time::Duration::from_millis(50)).await;
                        if let Err(e) = power::reboot().await {
                            log::error!("Failed to reboot: {e:#}");
                        }
                    })
                    .detach();
                }
                menu::MenuItem::RebootBootloader => {
                    self.log("Rebooting to bootloader...");
                    fasync::Task::local(async {
                        // give UI some time to show the log above
                        fasync::Timer::new(std::time::Duration::from_millis(50)).await;
                        if let Err(e) = power::reboot_to_bootloader().await {
                            log::error!("Failed to reboot to bootloader: {e:#}");
                        }
                    })
                    .detach();
                }
                menu::MenuItem::PowerOff => {
                    self.log("Powering off...");
                    fasync::Task::local(async {
                        // give UI some time to show the log above
                        fasync::Timer::new(std::time::Duration::from_millis(50)).await;
                        if let Err(e) = power::power_off().await {
                            log::error!("Failed to power off: {e:#}");
                        }
                    })
                    .detach();
                }
                menu_item => {
                    log::error!("Not implemented: {menu_item:?}");
                }
            },
            _ => {
                return Ok(());
            }
        }
        self.wheel_diff = 0;
        self.scene = None;
        context.request_render();
        Ok(())
    }
}

#[fuchsia::main]
fn main() -> Result<(), Error> {
    log::info!("recovery-android started.");

    let config = recovery_ui_config::Config::take_from_startup_handle();
    let display_rotation = match config.display_rotation {
        0 => DisplayRotation::Deg0,
        180 => DisplayRotation::Deg180,
        // Carnelian uses an inverted z-axis for rotation
        90 => DisplayRotation::Deg270,
        270 => DisplayRotation::Deg90,
        val => {
            log::error!("Invalid display_rotation {}, defaulting to 0 degrees", val);
            DisplayRotation::Deg0
        }
    };

    App::run(Box::new(move |_| {
        Box::pin(async move {
            let assistant = Box::new(RecoveryAppAssistant::new(display_rotation));
            Ok::<AppAssistantPtr, Error>(assistant)
        })
    }))
}
