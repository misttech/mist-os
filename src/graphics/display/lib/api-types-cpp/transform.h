// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_TRANSFORM_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_TRANSFORM_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <cstdint>

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/Transform`] and the
// banjo type [`fuchsia.hardware.display.controller/FrameTransform`].
//
// See `::fuchsia_hardware_display_types::wire::Transform` for references.
using Transform = fuchsia_hardware_display_types::wire::Transform;

Transform ToTransform(fuchsia_hardware_display_types::wire::Transform transform_fidl);
Transform ToTransform(frame_transform_t frame_transform_banjo);

fuchsia_hardware_display_types::wire::Transform ToFidlTransform(Transform transform);
frame_transform_t ToBanjoFrameTransform(Transform transform);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_TRANSFORM_H_
