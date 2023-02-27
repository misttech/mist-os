// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fmt::Debug, hash::Hash};

use fidl_fuchsia_math::*;
use indexmap::IndexMap;

use crate::{
    Frame, FrameState, HitArea, HitTarget, Layout, LayoutChange, LayoutError, Pointer,
    SetActiveFrame, BORDER_BOTTOM, BORDER_LEFT, BORDER_RIGHT, BORDER_TOP,
};

/// Defines a "Floating Windows" layout where `Frame`s are organized by the user on the screen.
#[derive(Debug)]
pub struct FloatingLayout<K, V>
where
    K: Copy + Debug + Eq + Hash,
    V: Frame<K>,
{
    /// The list of frames owned by this layout.
    frames: IndexMap<K, V>,
    /// The bounds of the layout itself under which it positions the frames.
    layout_rect: Rect,
    /// The default frame size to use if the `Frame` itself does not provide it.
    default_size: SizeU,
    /// The minimum frame size below which a resize operation has no effect.
    min_size: SizeU,
    /// The maximum frame size above which a resize operation has no effect.
    max_size: SizeU,
    /// Holds the target of a pointer hit test during move and resize operations.
    hit_target: Option<HitTarget<K>>,
}

impl<K, V> FloatingLayout<K, V>
where
    K: Copy + Debug + Eq + Hash,
    V: Frame<K>,
{
    /// Defines the value by which new frame is offset from previous frame in floating layout.
    pub const FLOATING_FRAME_OFFSET: u32 = 32;

    pub fn new() -> Self {
        FloatingLayout {
            frames: IndexMap::new(),
            layout_rect: Rect { x: 0, y: 0, width: 3840, height: 2048 },
            default_size: SizeU { width: 800, height: 600 },
            min_size: SizeU { width: 64, height: 64 },
            max_size: SizeU { width: u32::MAX, height: u32::MAX },
            hit_target: None,
        }
    }

    pub fn with_default_size(mut self, size: SizeU) -> Self {
        self.default_size = size;
        self
    }

    pub fn with_min_size(mut self, size: SizeU) -> Self {
        assert!(size.width <= self.default_size.width && size.height <= self.default_size.height);
        self.min_size = size;
        self
    }

    pub fn with_max_size(mut self, size: SizeU) -> Self {
        assert!(size.width > self.default_size.width && size.height > self.default_size.height);
        self.max_size = size;
        self
    }

    fn build_rect(&self, size: Option<SizeU>) -> RectF {
        let size = match size {
            Some(size) => SizeU {
                width: size.width.clamp(self.min_size.width, self.max_size.width),
                height: size.height.clamp(self.min_size.height, self.max_size.height),
            },
            None => self.default_size,
        };

        let mut x = self.frames.len() as i32 * Self::FLOATING_FRAME_OFFSET as i32;
        if x + size.width as i32 > self.layout_rect.width {
            x = self.layout_rect.width - size.width as i32;
        }

        let mut y = self.frames.len() as i32 * Self::FLOATING_FRAME_OFFSET as i32;
        if y + size.height as i32 > self.layout_rect.height {
            y = self.layout_rect.height - size.height as i32;
        }

        RectF { x: x as f32, y: y as f32, width: size.width as f32, height: size.height as f32 }
    }

    fn hittest(&self, point: PointF) -> Option<HitTarget<K>> {
        for frame in self.frames.values().rev() {
            let result = frame.hittest(point);
            if result.is_some() {
                return result;
            }
        }
        None
    }

    fn drag(&mut self, target: &mut HitTarget<K>, delta: PointF) -> Result<(), LayoutError<K>> {
        // Drag should be called only for hits on titlebar and borders.
        if target.hit_area == HitArea::Body {
            return Err(LayoutError::NoHit);
        }

        if delta.x == 0.0 && delta.y == 0.0 {
            return Err(LayoutError::NoHit);
        }

        if let Some(frame) = self.frames.get_mut(&target.id) {
            let mut frame_rect = frame.get_rect()?;
            if target.hit_area == HitArea::Titlebar {
                // Convert drag to move.
                frame_rect.x += delta.x;
                frame_rect.y += delta.y;
            } else if let HitArea::Border { edge } = target.hit_area {
                if edge & BORDER_LEFT != 0 {
                    frame_rect.x += delta.x;
                    if self.min_size.width < (frame_rect.width - delta.x) as u32 {
                        frame_rect.width -= delta.x;
                    }
                }

                if edge & BORDER_RIGHT != 0 {
                    if self.min_size.width < (frame_rect.width + delta.x) as u32 {
                        frame_rect.width += delta.x;
                    }
                }

                if edge & BORDER_TOP != 0 {
                    frame_rect.y += delta.y;
                    if self.min_size.height < (frame_rect.height - delta.y) as u32 {
                        frame_rect.height -= delta.y;
                    }
                }

                if edge & BORDER_BOTTOM != 0 {
                    if self.min_size.height < (frame_rect.height + delta.y) as u32 {
                        frame_rect.height += delta.y;
                    }
                }
            }

            frame.set_rect(frame_rect)?;
        }

        // Update hittarget.
        target.point.x += delta.x;
        target.point.y += delta.y;

        return Ok(());
    }
}

impl<K, V> Layout<K, V> for FloatingLayout<K, V>
where
    K: Copy + Debug + Eq + Hash,
    V: Frame<K>,
{
    fn add(&mut self, mut frame: V) -> Result<LayoutChange, LayoutError<K>> {
        // Set the new frame and mark it as active.
        let frame_rect = self.build_rect(frame.get_initial_size());
        frame.set_rect(frame_rect)?;
        frame.set_active(true)?;

        // Set the current active frame as inactive.
        self.frames.values_mut().last().and_then(|frame| frame.set_active(false).ok());

        self.frames.insert(frame.get_id(), frame);

        Ok(LayoutChange::DisplayOrder)
    }

    fn remove(&mut self, id: K) -> Result<LayoutChange, LayoutError<K>> {
        self.frames.remove(&id);

        // Set the last frame as active.
        self.frames.values_mut().last().and_then(|frame| frame.set_active(true).ok());

        Ok(LayoutChange::DisplayOrder)
    }

    fn clear(&mut self) {
        self.frames.clear();
    }

    fn set_rect(&mut self, rect: Rect) -> Result<LayoutChange, LayoutError<K>> {
        self.layout_rect = rect;
        Ok(LayoutChange::Appearance)
    }

    fn get_frame(&self, id: K) -> Option<&V> {
        self.frames.get(&id)
    }

    fn get_frame_mut(&mut self, id: K) -> Option<&mut V> {
        self.frames.get_mut(&id)
    }

    fn get_active(&self) -> Option<K> {
        self.frames.keys().last().copied()
    }

    fn set_active(&mut self, frame_id: SetActiveFrame<K>) -> Result<LayoutChange, LayoutError<K>> {
        // We need at least 2 frames to switch active frame.
        if self.frames.len() < 2 {
            return Ok(LayoutChange::None);
        }

        // Get the index of the new frame to mark active.
        if let Some(index) = match frame_id {
            SetActiveFrame::Frame(id) => self.frames.get_index_of(&id),
            SetActiveFrame::NextFrame => Some(0),
            SetActiveFrame::PrevFrame => Some(self.frames.len() - 2),
        } {
            // Nothing to do if this is already the active (last) frame.
            if index == self.frames.len() - 1 {
                return Ok(LayoutChange::None);
            }

            // Set the last frame as not active.
            if let Some((_, last_frame)) = self.frames.last_mut() {
                if last_frame.get_state() == FrameState::Fullscreen {
                    last_frame.set_state(FrameState::Normal)?;
                }
                last_frame.set_active(false)?;
            }

            // Move the new frame to the end of the list to make it active.
            match frame_id {
                SetActiveFrame::Frame(_) => self.frames.move_index(index, self.frames.len() - 1),
                SetActiveFrame::NextFrame => self.frames.move_index(0, self.frames.len() - 1),
                SetActiveFrame::PrevFrame => self.frames.move_index(self.frames.len() - 1, 0),
            }

            if let Some((_, frame)) = self.frames.last_mut() {
                frame.set_active(true)?;
            }

            Ok(LayoutChange::DisplayOrder)
        } else {
            Err(LayoutError::NoChange)
        }
    }

    fn get_state(&self, id: K) -> Option<FrameState> {
        self.frames.get(&id).map(|frame| frame.get_state())
    }

    fn set_state(&mut self, id: K, state: FrameState) -> Result<LayoutChange, LayoutError<K>> {
        if let Some(frame) = self.frames.get_mut(&id) {
            let mut layout_change = frame.set_state(state)?;
            // If state is fullscreen, make it active as well.
            if state == FrameState::Fullscreen {
                layout_change = self.set_active(SetActiveFrame::Frame(id))?;
            }

            Ok(layout_change)
        } else {
            Err(LayoutError::NotFound { id })
        }
    }

    fn on_pointer(&mut self, pointer: Pointer) -> Result<LayoutChange, LayoutError<K>> {
        match pointer {
            Pointer::Hover { point } => {
                let hit_target = self.hittest(point);
                match (hit_target, self.hit_target.take()) {
                    (None, None) => {}
                    (None, Some(old_target)) => {
                        self.hit_target = None;
                        if let Some(frame) = self.frames.get_mut(&old_target.id) {
                            return frame.hover(point, HitArea::Outside);
                        }
                    }
                    (Some(new_target), None) => {
                        if let Some(frame) = self.frames.get_mut(&new_target.id) {
                            let hit_area = new_target.hit_area;
                            self.hit_target = Some(new_target);
                            return frame.hover(point, hit_area);
                        }
                    }
                    (Some(new_target), Some(old_target)) => {
                        let mut layout_changed = LayoutChange::None;
                        if old_target.id != new_target.id {
                            if let Some(frame) = self.frames.get_mut(&old_target.id) {
                                layout_changed = frame.hover(point, HitArea::Outside)?;
                            }
                            if let Some(frame) = self.frames.get_mut(&new_target.id) {
                                layout_changed = frame.hover(point, new_target.hit_area)?;
                            }
                        } else if old_target.hit_area != new_target.hit_area {
                            if let Some(frame) = self.frames.get_mut(&new_target.id) {
                                layout_changed = frame.hover(point, new_target.hit_area)?;
                            }
                        }
                        self.hit_target = Some(new_target);
                        return Ok(layout_changed);
                    }
                }
            }
            Pointer::Down { point } => {
                if let Some(hit_target) = self.hittest(point) {
                    if let Some(frame) = self.frames.get_mut(&hit_target.id) {
                        frame.hover(point, hit_target.hit_area)?;
                        let layout_changed =
                            self.set_active(SetActiveFrame::Frame(hit_target.id))?;
                        self.hit_target = Some(hit_target);
                        return Ok(layout_changed);
                    }
                } else {
                    self.hit_target = None;
                }
            }
            Pointer::Move { delta, .. } => {
                if let Some(mut hit_target) = self.hit_target.take() {
                    self.drag(&mut hit_target, delta)?;
                    self.hit_target = Some(hit_target);
                    return Ok(LayoutChange::Appearance);
                }
            }
            Pointer::Up { .. } | Pointer::Cancel => {
                if let Some(hit_target) = self.hit_target.take() {
                    if let Some(frame) = self.frames.get_mut(&hit_target.id) {
                        return frame.hover(
                            PointF { x: f32::NEG_INFINITY, y: f32::NEG_INFINITY },
                            HitArea::Outside,
                        );
                    }
                }
            }
        }
        Err(LayoutError::NoHit)
    }

    fn iter(&self) -> Box<dyn Iterator<Item = &V> + '_> {
        Box::new(self.frames.values())
    }
}
