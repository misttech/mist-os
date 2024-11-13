// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/rectangle.h"

#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace display {

static_assert(Rectangle::kMaxImageWidth == ImageMetadata::kMaxWidth);
static_assert(Rectangle::kMaxImageHeight == ImageMetadata::kMaxHeight);

}  // namespace display
