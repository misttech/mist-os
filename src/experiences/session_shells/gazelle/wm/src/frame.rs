// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use appkit::{srgb_to_linear, ChildView, ChildViewId, Window};
use fidl_fuchsia_math as fmath;
use fidl_fuchsia_ui_composition as ui_comp;
use layout::{
    Alignment, Frame, FrameState, FuchsiaMathRectExt, HitArea, HitTarget, LayoutChange, LayoutError,
};
use lazy_static::lazy_static;

lazy_static! {
    static ref TITLEBAR_ACTIVE_COLOR: ui_comp::ColorRgba = srgb_to_linear(0x546E7AFF);
    static ref TITLEBAR_INACTIVE_COLOR: ui_comp::ColorRgba = srgb_to_linear(0x2E3D44FF);
    static ref FRAME_BACKGROUND_COLOR: ui_comp::ColorRgba = srgb_to_linear(0x171F2380);
}

#[derive(Debug)]
pub struct WindowFrame {
    id: ChildViewId,
    flatland: ui_comp::FlatlandProxy,
    child_view: Option<ChildView>,
    frame_transform: ui_comp::TransformId,
    frame_background_transform: ui_comp::TransformId,
    hit_region_transform: ui_comp::TransformId,
    child_view_transform: ui_comp::TransformId,
    frame_background: ui_comp::ContentId,
    titlebar_background: ui_comp::ContentId,
    bounds: fmath::RectF,
    state: FrameState,
    active: bool,
    resizing: bool,
}

impl WindowFrame {
    pub const TITLEBAR_HEIGHT: u32 = 32;
    pub const BORDER_THICKNESS: i32 = 10;

    pub fn new(id: ChildViewId, window: &mut Window) -> Result<Self, Error> {
        let flatland = window.get_flatland();

        // Create the root transform for the child view frame.
        let mut frame_transform = window.create_transform(None)?;

        // Create the child titlebar transform and add to frame transform.
        let frame_background_transform = window.create_transform(Some(&mut frame_transform))?;

        // Create a solid fill frame background.
        let mut frame_background = window.next_content_id();
        flatland.create_filled_rect(&mut frame_background)?;
        window.set_content(frame_background_transform, frame_background)?;

        // Create the child titlebar transform and add to frame transform.
        let titlebar_transform = window.create_transform(Some(&mut frame_transform))?;

        // Create a solid fill titlebar background.
        let mut titlebar_background = window.next_content_id();
        flatland.create_filled_rect(&mut titlebar_background)?;
        window.set_content(titlebar_transform, titlebar_background)?;

        // Create the child view transform and add to frame transform.
        let mut child_view_transform = window.create_transform(Some(&mut frame_transform))?;
        flatland.set_translation(
            &mut child_view_transform,
            &mut fmath::Vec_ { x: 0, y: WindowFrame::TITLEBAR_HEIGHT as i32 },
        )?;

        // Finally, create a hit-region transform that covers the child_view_transform.
        let mut hit_region_transform = window.create_transform(Some(&mut frame_transform))?;
        flatland.set_translation(
            &mut hit_region_transform,
            &mut fmath::Vec_ { x: 0, y: WindowFrame::TITLEBAR_HEIGHT as i32 },
        )?;

        Ok(WindowFrame {
            id,
            flatland,
            child_view: None,
            frame_transform,
            frame_background_transform,
            hit_region_transform,
            child_view_transform,
            frame_background,
            titlebar_background,
            bounds: fmath::RectF { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
            state: FrameState::Normal,
            active: false,
            resizing: false,
        })
    }

    pub fn get_frame_transform(&self) -> ui_comp::TransformId {
        self.frame_transform.clone()
    }

    pub fn get_child_view(&self) -> Option<&ChildView> {
        self.child_view.as_ref()
    }

    pub fn get_child_view_mut(&mut self) -> Option<&mut ChildView> {
        self.child_view.as_mut()
    }

    pub fn set_child_view(&mut self, child_view: ChildView) -> Result<(), Error> {
        self.flatland
            .set_content(&mut self.child_view_transform, &mut child_view.get_content_id())?;
        self.child_view = Some(child_view);
        Ok(())
    }

    fn draw(&mut self) -> Result<LayoutChange, Error> {
        // Title bar.
        let mut titlebar_color = if self.active {
            TITLEBAR_ACTIVE_COLOR.clone()
        } else {
            TITLEBAR_INACTIVE_COLOR.clone()
        };

        self.flatland.set_solid_fill(
            &mut self.titlebar_background,
            &mut titlebar_color,
            &mut fmath::SizeU {
                width: self.bounds.width as u32,
                height: WindowFrame::TITLEBAR_HEIGHT,
            },
        )?;

        // Frame background.
        if self.resizing {
            // Inflate frame background to surround the frame with a border.
            self.flatland.set_translation(
                &mut self.frame_background_transform,
                &mut fmath::Vec_ {
                    x: -WindowFrame::BORDER_THICKNESS,
                    y: -WindowFrame::BORDER_THICKNESS,
                },
            )?;

            self.flatland.set_solid_fill(
                &mut self.frame_background,
                &mut FRAME_BACKGROUND_COLOR.clone(),
                &mut fmath::SizeU {
                    width: self.bounds.width as u32
                        + 2 * WindowFrame::BORDER_THICKNESS.abs() as u32,
                    height: self.bounds.height as u32
                        + 2 * WindowFrame::BORDER_THICKNESS.abs() as u32,
                },
            )?;
        } else {
            self.flatland.set_translation(
                &mut self.frame_background_transform,
                &mut fmath::Vec_ { x: 0, y: 0 },
            )?;

            self.flatland.set_solid_fill(
                &mut self.frame_background,
                &mut FRAME_BACKGROUND_COLOR.clone(),
                &mut fmath::SizeU {
                    width: self.bounds.width as u32,
                    height: self.bounds.height as u32,
                },
            )?;

            if let Some(child_view) = self.child_view.as_mut() {
                child_view.set_size(
                    self.bounds.width as u32,
                    self.bounds.height as u32 - WindowFrame::TITLEBAR_HEIGHT,
                )?;
            }
        }

        // Clip bounds.
        self.flatland.set_clip_boundary(
            &mut self.child_view_transform,
            Some(&mut fmath::Rect {
                x: 0,
                y: 0,
                width: self.bounds.width as i32,
                height: self.bounds.height as i32 - WindowFrame::TITLEBAR_HEIGHT as i32,
            }),
        )?;

        Ok(LayoutChange::Appearance)
    }
}

impl Frame<ChildViewId> for WindowFrame {
    fn get_id(&self) -> ChildViewId {
        self.id
    }

    fn get_parent(&self) -> Option<ChildViewId> {
        None
    }

    fn get_alignment(&self) -> Option<Alignment> {
        None
    }

    fn get_initial_size(&self) -> Option<fmath::SizeU> {
        None
    }

    fn get_rect(&self) -> Result<fmath::RectF, LayoutError<ChildViewId>> {
        if let FrameState::Minimized = self.state {
            Ok(fmath::RectF { x: 0.0, y: 0.0, width: 0.0, height: 0.0 })
        } else {
            Ok(self.bounds)
        }
    }

    fn set_rect(&mut self, rect: fmath::RectF) -> Result<LayoutChange, LayoutError<ChildViewId>> {
        // Set position.
        if self.bounds.x != rect.x || self.bounds.y != rect.y {
            self.bounds.x = rect.x;
            self.bounds.y = rect.y;

            self.flatland
                .set_translation(
                    &mut self.frame_transform,
                    &mut fmath::Vec_ { x: rect.x as i32, y: rect.y as i32 },
                )
                .map_err(|source| LayoutError::<ChildViewId>::Failed { source: source.into() })?;
        }

        // Set size and color of titlebar, child view and clip bounds.
        if self.bounds.width != rect.width || self.bounds.height != rect.height {
            self.bounds.width = rect.width;
            self.bounds.height = rect.height;

            self.draw()
                .map_err(|source| LayoutError::<ChildViewId>::Failed { source: source.into() })?;
        }

        Ok(LayoutChange::Appearance)
    }

    fn get_state(&self) -> FrameState {
        self.state
    }

    fn set_state(&mut self, state: FrameState) -> Result<LayoutChange, LayoutError<ChildViewId>> {
        self.state = state;
        // When fullscreen, remove the child view's content from this frame, it will be moved
        // to the window manager's fullscreen view transform.
        if let FrameState::Fullscreen = state {
            self.flatland
                .set_content(&mut self.child_view_transform, &mut ui_comp::ContentId { value: 0 })
                .map_err(|source| LayoutError::<ChildViewId>::Failed { source: source.into() })?;
        } else if let Some(child_view) = self.child_view.as_mut() {
            self.flatland
                .set_content(&mut self.child_view_transform, &mut child_view.get_content_id())
                .map_err(|source| LayoutError::<ChildViewId>::Failed { source: source.into() })?;
            self.draw()
                .map_err(|source| LayoutError::<ChildViewId>::Failed { source: source.into() })?;
        }
        Ok(LayoutChange::Appearance)
    }

    fn set_active(&mut self, active: bool) -> Result<LayoutChange, LayoutError<ChildViewId>> {
        if self.active != active {
            self.active = active;

            // Set the size and color of the titlebar.
            let mut titlebar_color = if active {
                TITLEBAR_ACTIVE_COLOR.clone()
            } else {
                TITLEBAR_INACTIVE_COLOR.clone()
            };
            self.flatland
                .set_solid_fill(
                    &mut self.titlebar_background,
                    &mut titlebar_color,
                    &mut fmath::SizeU {
                        width: self.bounds.width as u32,
                        height: WindowFrame::TITLEBAR_HEIGHT,
                    },
                )
                .map_err(|source| LayoutError::<ChildViewId>::Failed { source: source.into() })?;

            // Also enable/disable interaction based on active state.
            if active {
                // Remove the hittest region above the child view to make it interactive.
                self.flatland.set_hit_regions(&mut self.hit_region_transform, &[]).map_err(
                    |source| LayoutError::<ChildViewId>::Failed { source: source.into() },
                )?;
            } else {
                // Add a hittest region above the child view to prevent interaction with it.
                self.flatland
                    .set_infinite_hit_region(
                        &mut self.hit_region_transform,
                        ui_comp::HitTestInteraction::Default,
                    )
                    .map_err(|source| LayoutError::<ChildViewId>::Failed {
                        source: source.into(),
                    })?;
            }
            Ok(LayoutChange::Appearance)
        } else {
            Err(LayoutError::NoChange)
        }
    }

    fn hittest(&self, point: fmath::PointF) -> Option<HitTarget<ChildViewId>> {
        // Apply border thickness to get frame and body rects.
        let (frame_rect, body_rect) = if WindowFrame::BORDER_THICKNESS > 0 {
            (self.bounds.inflate(WindowFrame::BORDER_THICKNESS as f32), self.bounds)
        } else {
            (self.bounds, self.bounds.inflate(WindowFrame::BORDER_THICKNESS as f32))
        };

        match layout::hittest(point, frame_rect, body_rect, WindowFrame::TITLEBAR_HEIGHT) {
            HitArea::Outside => None,
            hit_area => Some(HitTarget { id: self.id, hit_area, point, hit_data: None }),
        }
    }

    fn hover(
        &mut self,
        _point: fmath::PointF,
        hit_area: HitArea,
    ) -> Result<LayoutChange, LayoutError<ChildViewId>> {
        let resizing = matches!(hit_area, HitArea::Border { .. });
        if self.resizing != resizing {
            self.resizing = resizing;
            // Show / hide resize borders.
            self.draw()
                .map_err(|source| LayoutError::<ChildViewId>::Failed { source: source.into() })
        } else {
            Ok(LayoutChange::None)
        }
    }
}
