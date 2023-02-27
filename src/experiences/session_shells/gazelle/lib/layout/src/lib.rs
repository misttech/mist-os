// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod helpers;
mod layout;

use std::{any::Any, fmt::Debug, hash::Hash};

pub use fidl_fuchsia_math::*;
pub use helpers::*;
use thiserror::Error;

pub use crate::layout::FloatingLayout;

/// Defines a trait for a layout algorithm that organizes `Frame`'s on the screen.
///
/// This trait defines a common interface for various concrete layout strategies like: floating,
/// tiled or relative layouts.
///
/// The frames are organized in a display order, back-to-front. The last frame in the layout is,
/// by definition, the 'active' frame since it is the top-most in the display stack.
pub trait Layout<K, V>
where
    K: Copy + Debug + Eq + Hash,
    V: Frame<K>,
{
    /// Add a frame to the layout.
    fn add(&mut self, frame: V) -> Result<LayoutChange, LayoutError<K>>;
    /// Remove a frame from the layout.
    fn remove(&mut self, id: K) -> Result<LayoutChange, LayoutError<K>>;
    /// Remove all frames from the layout.
    fn clear(&mut self);
    /// Set the dimensions of the layout itself. This may rearrange frames in the layout.
    fn set_rect(&mut self, rect: Rect) -> Result<LayoutChange, LayoutError<K>>;
    /// Returns a reference to the frame id in the layout, if present. None otherwise.
    fn get_frame(&self, id: K) -> Option<&V>;
    /// Returns a mutable reference to the frame in the layout, if present. None otherwise.
    fn get_frame_mut(&mut self, id: K) -> Option<&mut V>;
    /// Returns the frame id of the active (last) frame in the layout.
    fn get_active(&self) -> Option<K>;
    /// Sets a frame, in `frame_id`, the active frame in the layout.
    fn set_active(&mut self, frame_id: SetActiveFrame<K>) -> Result<LayoutChange, LayoutError<K>>;
    /// Returns the `FrameState` of a frame with `id` in the layout.
    fn get_state(&self, id: K) -> Option<FrameState>;
    /// Sets the state of a specific frame in the layout.
    fn set_state(&mut self, id: K, state: FrameState) -> Result<LayoutChange, LayoutError<K>>;
    /// Handles pointer events and updates the layout.
    fn on_pointer(&mut self, pointer: Pointer) -> Result<LayoutChange, LayoutError<K>>;
    /// Returns an iterator for all frames present in the layout.
    fn iter(&self) -> Box<dyn Iterator<Item = &V> + '_>;
}

/// Defines a trait for a rectangular region that is placed inside a [Layout].
pub trait Frame<K>: Debug
where
    K: Copy + Debug + Eq + Hash,
{
    /// Returns an id for this frame.
    fn get_id(&self) -> K;
    /// Returns an optional id of a parent frame, to which this frame is associated with.
    /// This is used for relative layouts.
    fn get_parent(&self) -> Option<K>;
    /// Returns an optional alignment metrics of this LayoutRect.
    /// This is used for relative layouts.
    fn get_alignment(&self) -> Option<Alignment>;
    /// Return an optional initial size of the frame in the layout's coordinate space.
    fn get_initial_size(&self) -> Option<SizeU>;
    /// Get the current position and dimension of this frame in layout's coordinate space.
    fn get_rect(&self) -> Result<RectF, LayoutError<K>>;
    /// Set the position and dimension of this frame in layout's coordinate space.
    fn set_rect(&mut self, rect: RectF) -> Result<LayoutChange, LayoutError<K>>;
    /// Returns the current frame state.
    fn get_state(&self) -> FrameState;
    /// Set the frame state.
    fn set_state(&mut self, state: FrameState) -> Result<LayoutChange, LayoutError<K>>;
    /// Make this frame active/inactive.
    fn set_active(&mut self, active: bool) -> Result<LayoutChange, LayoutError<K>>;
    /// Returns self as hittest target for `point`, None otherwise.
    fn hittest(&self, point: PointF) -> Option<HitTarget<K>>;
    /// Apply pointer hover effects to this frame.
    fn hover(&mut self, point: PointF, hit_area: HitArea) -> Result<LayoutChange, LayoutError<K>>;
}

/// Defines an enumeration of all possible states a frame can be in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameState {
    Normal,
    Minimized,
    Maximized,
    Fullscreen,
}

/// Defines the set of changes caused by any mutation to the layout.
#[derive(Debug, Clone)]
pub enum LayoutChange {
    /// No change to display order or appearance.
    None,
    /// The change resulted in re-ordering of frames in the layout.
    DisplayOrder,
    /// The change resulted in update to the frame's visual state.
    Appearance,
}

/// Defines the possible set of error results returned from `Layout` methods.
#[derive(Debug, Error)]
pub enum LayoutError<T>
where
    T: Copy + Debug + Eq + Hash,
{
    #[error("Layout rect with id {:?} not found", id)]
    NotFound { id: T },
    #[error("Invalid size supplied")]
    InvalidSize,
    #[error("Hittest found no hit")]
    NoHit,
    #[error("No change in layout")]
    NoChange,
    #[error("Failed to layout")]
    Failed { source: anyhow::Error },
}

/// Defines an enumeration for setting an active frame.
pub enum SetActiveFrame<T>
where
    T: Copy + Debug + Eq + Hash,
{
    /// Mark frame with id as active.
    Frame(T),
    /// Mark the next frame to the current active frame as active. Since an active frame is the last
    /// frame in the list, the next frame would be at index 0.
    NextFrame,
    /// Mark the previous frame to the current active frame as active. Since an active frame is the
    /// last frame in the list, the previous frame would be second to last in the list.
    PrevFrame,
}

/// Defines a bitflag for edges of the frame border.
pub type BorderEdge = u32;
pub const BORDER_NONE: BorderEdge = 0x0000;
pub const BORDER_LEFT: BorderEdge = 0x0001;
pub const BORDER_TOP: BorderEdge = 0x0002;
pub const BORDER_RIGHT: BorderEdge = 0x0004;
pub const BORDER_BOTTOM: BorderEdge = 0x0008;
pub const BORDER_TOP_LEFT: BorderEdge = BORDER_TOP | BORDER_LEFT;
pub const BORDER_TOP_RIGHT: BorderEdge = BORDER_TOP | BORDER_RIGHT;
pub const BORDER_BOTTOM_LEFT: BorderEdge = BORDER_BOTTOM | BORDER_LEFT;
pub const BORDER_BOTTOM_RIGHT: BorderEdge = BORDER_BOTTOM | BORDER_RIGHT;

/// Defines a set of possible hit areas returned from a hit test.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum HitArea {
    Titlebar,
    Border { edge: BorderEdge },
    Body,
    Outside,
}

/// Defines a set of pointer events that are handled by the `Layout`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Pointer {
    Hover { point: PointF },
    Down { point: PointF },
    Move { point: PointF, delta: PointF },
    Up { point: PointF },
    Cancel,
}

/// Defines an object returned from a hit test.
#[derive(Debug)]
pub struct HitTarget<T>
where
    T: Copy + Debug + Eq + Hash,
{
    /// The frame id that was hit.
    pub id: T,
    /// The hit area.
    pub hit_area: HitArea,
    /// The point where hit occurred.
    pub point: PointF,
    /// Optional data.
    pub hit_data: Option<Box<dyn Any>>,
}

/// Defines alignment metrics supplied by [Frame] to a [Layout].
///
/// These are primarily used by 'relative' layout, where a src frame is aligned with a dst frame.
#[derive(Clone, Copy, Debug)]
pub struct Alignment {
    /// A normalized point (0.0..1.0) on the source LayoutRect being aligned.
    pub src_point: PointF,
    /// A normalized point (0.0..1.0) on the destination LayoutRect.
    pub dst_point: PointF,
    /// Additional offset to add to the mapped alignment point on the destination LayoutRect.
    pub offset: Point,
    /// True if the source LayoutRect stays anchored to the destination LayoutRect.
    pub is_anchored: bool,
}
