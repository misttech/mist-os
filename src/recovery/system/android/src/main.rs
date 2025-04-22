// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use carnelian::color::Color;
use carnelian::drawing::{DisplayRotation, FontFace};
use carnelian::render::rive::load_rive;
use carnelian::scene::facets::{RiveFacet, TextFacetOptions, TextHorizontalAlignment};
use carnelian::scene::layout::{
    CrossAxisAlignment, Flex, FlexOptions, MainAxisAlignment, MainAxisSize,
};
use carnelian::scene::scene::{Scene, SceneBuilder};
use carnelian::{
    App, AppAssistant, AppAssistantPtr, Point, Size, ViewAssistant, ViewAssistantContext,
    ViewAssistantPtr, ViewKey,
};
use euclid::size2;

const LOGO_IMAGE_PATH: &str = "/system-recovery-config/logo.riv";
const BG_COLOR: Color = Color::new(); // Black
const HEADER_COLOR: Color = Color { r: 249, g: 194, b: 0, a: 255 };

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
}

impl RecoveryViewAssistant {
    fn new(view_key: ViewKey) -> Result<RecoveryViewAssistant, Error> {
        let font_face = recovery_ui::font::get_default_font_face().clone();
        let logo_file = load_rive(LOGO_IMAGE_PATH).ok();

        Ok(RecoveryViewAssistant { view_key, font_face, logo_file, scene: None })
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
                },
            );

            builder.build()
        }))
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
