// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

#include <type_traits>

namespace display {

static_assert(std::is_standard_layout_v<PixelFormat>);
static_assert(std::is_trivially_assignable_v<PixelFormat, PixelFormat>);
static_assert(std::is_trivially_copyable_v<PixelFormat>);
static_assert(std::is_trivially_copy_constructible_v<PixelFormat>);
static_assert(std::is_trivially_destructible_v<PixelFormat>);
static_assert(std::is_trivially_move_assignable_v<PixelFormat>);
static_assert(std::is_trivially_move_constructible_v<PixelFormat>);

}  // namespace display
