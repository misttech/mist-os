// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <type_traits>

namespace display {

static_assert(std::is_standard_layout_v<ImageTilingType>);
static_assert(std::is_trivially_assignable_v<ImageTilingType, ImageTilingType>);
static_assert(std::is_trivially_copyable_v<ImageTilingType>);
static_assert(std::is_trivially_copy_constructible_v<ImageTilingType>);
static_assert(std::is_trivially_destructible_v<ImageTilingType>);
static_assert(std::is_trivially_move_assignable_v<ImageTilingType>);
static_assert(std::is_trivially_move_constructible_v<ImageTilingType>);

// Ensure that the Banjo constants match the FIDL constants.
static_assert(ImageTilingType::kLinear.ToBanjo() == IMAGE_TILING_TYPE_LINEAR);
static_assert(ImageTilingType::kCapture.ToBanjo() == IMAGE_TILING_TYPE_CAPTURE);

}  // namespace display
