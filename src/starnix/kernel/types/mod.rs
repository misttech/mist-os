// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod arc_key;
mod file_mode;
mod mount_flags;
mod open_flags;
mod resource_limits;
mod seal_flags;
mod stats;
mod union;

pub mod as_any;
pub mod auth;
pub mod device_type;
pub mod errno;
pub mod kcmp;
pub mod ownership;
pub mod personality;
pub mod range_ext;
pub mod signals;
pub mod time;
pub mod uapi;
pub mod user_address;
pub mod user_buffer;

pub(crate) use union::struct_with_union_into_bytes;

pub use arc_key::{ArcKey, PtrKey, WeakKey};
pub use device_type::{
    DeviceType, DYN_MAJOR, INPUT_MAJOR, KEYBOARD_INPUT_MINOR, LOOP_MAJOR, TOUCH_INPUT_MINOR,
    TTY_ALT_MAJOR,
};
pub(crate) use file_mode::mode;
pub use file_mode::{Access, FileMode};
pub use mount_flags::MountFlags;
pub use open_flags::OpenFlags;
pub use ownership::{
    debug_assert_no_local_temp_ref, OwnedRef, OwnedRefByRef, Releasable, ReleasableByRef,
    ReleaseGuard, TempRef, WeakRef,
};
pub(crate) use ownership::{release_after, release_on_error};
pub use resource_limits::{Resource, ResourceLimits};
pub use seal_flags::SealFlags;
pub use stats::TaskTimeStats;
pub use uapi::*;
