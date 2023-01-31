// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

pub(super) const SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM: f32 = 6.0 / 12.0;
pub(super) const MAX_SPURIOUS_TO_INTENTIONAL_SCROLL_THRESHOLD_MM: f32 =
    5.0 * SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM;
pub(super) const TAP_TIMEOUT: zx::Duration = zx::Duration::from_millis(1200);
pub(super) const MAX_SCROLL_DIRECTION_SKEW_DEGREES: f32 = 40.0;

/// Based on palm / thumb size data collected from adults.
///
/// Mean of the max of contact size of thumb sequence is 4.878mm,
/// std of the max of contact size of thumb is 0.4836.
/// So we believe 5.8mm maybe a threshold of thumb to palm.
pub(super) const MIN_PALM_SIZE_MM: f32 = 5.8;

pub(super) const SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM: f32 =
    3.0 * SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM;
pub(super) const BUTTON_CHANGE_STATE_TIMEOUT: zx::Duration = zx::Duration::from_millis(100);
