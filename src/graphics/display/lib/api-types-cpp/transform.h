// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_TRANSFORM_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_TRANSFORM_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

namespace display {

// Equivalent to the FIDL type
// [`fuchsia.hardware.display.types/CoordinateTransformation`] and the banjo
// type [`fuchsia.hardware.display.controller/FrameTransform`].
//
// See `::fuchsia_hardware_display_types::wire::CoordinateTransformation` for references.
using Transform = fuchsia_hardware_display_types::wire::CoordinateTransformation;

Transform ToTransform(
    fuchsia_hardware_display_types::wire::CoordinateTransformation transform_fidl);
Transform ToTransform(coordinate_transformation_t transform_banjo);

fuchsia_hardware_display_types::wire::CoordinateTransformation ToFidlTransform(Transform transform);
coordinate_transformation_t ToBanjoFrameTransform(Transform transform);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_TRANSFORM_H_
