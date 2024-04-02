// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

///! Implements a double-buffer swapchain runner to display a Scene. The swapchain is represented
///! by two images that alternate in their assignment to a primary layer. Writes to each buffer and
///! the resulting swap is synchronized using each configuration's retirement fence, which is
///! aligned to the display's vsync events by the display driver.
use {
    anyhow::Result,
    display_utils::{
        Coordinator, DisplayConfig, DisplayId, Event, Image, ImageId, ImageParameters, Layer,
        LayerConfig, LayerId, PixelFormat,
    },
    fuchsia_trace::duration,
    std::{borrow::Borrow, io::Write},
};

use crate::{draw::MappedImage, fps::Counter};

// ANSI X3.64 (ECMA-48) escape code for clearing the terminal screen.
const CLEAR: &str = "\x1B[2K\r";

// A scene whose contents may change over time and can be rendered into
// images mapped to the address space.
pub trait Scene {
    // Update the scene contents.
    fn update(&mut self) -> Result<()>;

    // Initialize the image for rendering.
    // Invoked exactly once for every frame buffer image before it's used for
    // rendering for the first time.
    fn init_image(&self, image: &mut MappedImage) -> Result<()>;

    // Render the current scene contents to `image`.
    // `image` must not be used by the display engine during `render()`.
    fn render(&mut self, image: &mut MappedImage) -> Result<()>;
}

struct Presentation {
    image: MappedImage,
    retirement_event: Event,
}

impl Presentation {
    pub fn new(image: MappedImage, retirement_event: Event) -> Self {
        Presentation { image, retirement_event }
    }
}

pub struct DoubleBufferedFenceLoop<'a, S: Scene> {
    coordinator: &'a Coordinator,
    display_id: DisplayId,
    layer_id: LayerId,

    params: ImageParameters,

    scene: S,
    presentations: Vec<Presentation>,
}

impl<'a, S: Scene> DoubleBufferedFenceLoop<'a, S> {
    pub async fn new(
        coordinator: &'a Coordinator,
        display_id: DisplayId,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        scene: S,
    ) -> Result<Self> {
        let params = ImageParameters {
            width,
            height,
            pixel_format,
            color_space: fidl_fuchsia_sysmem::ColorSpaceType::Srgb,
            name: Some("image layer".to_string()),
        };
        let mut next_image_id = ImageId(1);

        const NUM_SWAPCHAIN_IMAGES: usize = 2;
        let mut image_presentations = Vec::new();
        for _ in 0..NUM_SWAPCHAIN_IMAGES {
            next_image_id = ImageId(next_image_id.0 + 1);

            let mut image = MappedImage::create(
                Image::create(coordinator.clone(), next_image_id, &params).await?,
            )?;
            let retire_event = coordinator.create_event()?;
            scene.init_image(&mut image)?;
            image_presentations.push(Presentation::new(image, retire_event));
        }

        let layer_id = coordinator.create_layer().await?;

        Ok(DoubleBufferedFenceLoop {
            coordinator,
            display_id,
            layer_id,
            params,

            scene,
            presentations: image_presentations,
        })
    }

    fn build_display_configs(&self, presentation_index: usize) -> Vec<DisplayConfig> {
        let presentation = &self.presentations[presentation_index];
        vec![DisplayConfig {
            id: self.display_id,
            layers: vec![Layer {
                id: self.layer_id,
                config: LayerConfig::Primary {
                    image_id: presentation.image.id(),
                    image_metadata: self.params.borrow().into(),
                    unblock_event: None,
                    retirement_event: Some(presentation.retirement_event.id()),
                },
            }],
        }]
    }

    pub async fn run(&mut self) -> Result<()> {
        // Apply the first config.
        let mut current_config = 0;
        self.coordinator.apply_config(&self.build_display_configs(current_config)).await?;

        let mut counter = Counter::new();
        loop {
            // Log the frame rate.
            counter.add(fuchsia_zircon::Time::get_monotonic());
            let stats = counter.stats();
            print!(
                "{}Display {:.2} fps ({:.5} ms)",
                CLEAR, stats.sample_rate_hz, stats.sample_time_delta_ms
            );
            std::io::stdout().flush()?;

            // Prepare the next image.
            // `current_config` alternates between 0 and 1.
            current_config ^= 1;
            let current_presentation = &mut self.presentations[current_config];

            {
                duration!(c"gfx", c"frame", "id" => stats.num_frames);
                {
                    duration!(c"gfx", c"update scene");
                    self.scene.update()?;
                }

                // Render the scene into the current presentation.
                {
                    duration!(c"gfx", c"render frame", "image" => current_config as u32);
                    self.scene.render(&mut current_presentation.image)?;
                }

                // Request the swap.
                {
                    duration!(c"gfx", c"apply config");
                    self.coordinator
                        .apply_config(&self.build_display_configs(current_config))
                        .await?;
                }
            }

            // Wait for the previous frame image to retire before drawing on it.
            let previous_presentation = &self.presentations[current_config ^ 1];
            previous_presentation.retirement_event.wait().await?;
        }
    }
}
