// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_math::*;

use crate::{HitArea, BORDER_BOTTOM, BORDER_LEFT, BORDER_NONE, BORDER_RIGHT, BORDER_TOP};

pub trait FuchsiaMathRectExt {
    fn left(&self) -> f32;
    fn top(&self) -> f32;
    fn right(&self) -> f32;
    fn bottom(&self) -> f32;
    fn contains(&self, point: PointF) -> bool;
    fn center(&self) -> PointF;
    fn inset(&self, inset: Inset) -> RectF;
    fn inflate(&self, value: f32) -> RectF;
}

impl FuchsiaMathRectExt for RectF {
    #[inline]
    fn contains(&self, point: PointF) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }

    #[inline]
    fn center(&self) -> PointF {
        PointF { x: self.x + self.width / 2.0, y: self.y + self.height / 2.0 }
    }

    #[inline]
    fn left(&self) -> f32 {
        self.x
    }

    #[inline]
    fn right(&self) -> f32 {
        self.x + self.width
    }
    #[inline]
    fn top(&self) -> f32 {
        self.y
    }
    #[inline]
    fn bottom(&self) -> f32 {
        self.y + self.height
    }

    #[inline]
    fn inset(&self, inset: Inset) -> RectF {
        RectF {
            x: self.x + inset.left as f32,
            y: self.y + inset.top as f32,
            width: self.width - (inset.left + inset.right) as f32,
            height: self.height - (inset.top + inset.bottom) as f32,
        }
    }

    #[inline]
    fn inflate(&self, value: f32) -> RectF {
        RectF {
            x: self.x - value,
            y: self.y - value,
            width: self.width + (2.0 * value),
            height: self.height + (2.0 * value),
        }
    }
}

#[inline]
pub fn hittest(
    point: PointF,
    frame_rect: RectF,
    body_rect: RectF,
    titlebar_height: u32,
) -> HitArea {
    if frame_rect.contains(point) {
        if !body_rect.contains(point) {
            let mut edge = BORDER_NONE;
            if point.x <= body_rect.left() {
                edge |= BORDER_LEFT;
            } else if point.x > body_rect.right() {
                edge |= BORDER_RIGHT;
            }
            if point.y <= body_rect.top() {
                edge |= BORDER_TOP;
            } else if point.y > body_rect.bottom() {
                edge |= BORDER_BOTTOM;
            }
            HitArea::Border { edge }
        } else if point.y < body_rect.y + titlebar_height as f32 {
            HitArea::Titlebar
        } else {
            HitArea::Body
        }
    } else {
        HitArea::Outside
    }
}
