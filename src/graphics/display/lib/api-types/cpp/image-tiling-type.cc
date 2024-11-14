// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

namespace display {

// Ensure that the Banjo constants match the FIDL constants.
static_assert(ImageTilingType::kLinear.ToBanjo() == IMAGE_TILING_TYPE_LINEAR);
static_assert(ImageTilingType::kCapture.ToBanjo() == IMAGE_TILING_TYPE_CAPTURE);

}  // namespace display
