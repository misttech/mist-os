// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/layer-composition-operations.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

namespace display {

// Ensure that the Banjo constants match the FIDL constants.
static_assert(LayerCompositionOperations::kUseImage.ToBanjo() ==
              LAYER_COMPOSITION_OPERATIONS_USE_IMAGE);
static_assert(LayerCompositionOperations::kMergeBase.ToBanjo() ==
              LAYER_COMPOSITION_OPERATIONS_MERGE_BASE);
static_assert(LayerCompositionOperations::kMergeSrc.ToBanjo() ==
              LAYER_COMPOSITION_OPERATIONS_MERGE_SRC);
static_assert(LayerCompositionOperations::kFrameScale.ToBanjo() ==
              LAYER_COMPOSITION_OPERATIONS_FRAME_SCALE);
static_assert(LayerCompositionOperations::kSrcFrame.ToBanjo() ==
              LAYER_COMPOSITION_OPERATIONS_SRC_FRAME);
static_assert(LayerCompositionOperations::kTransform.ToBanjo() ==
              LAYER_COMPOSITION_OPERATIONS_TRANSFORM);
static_assert(LayerCompositionOperations::kColorConversion.ToBanjo() ==
              LAYER_COMPOSITION_OPERATIONS_COLOR_CONVERSION);
static_assert(LayerCompositionOperations::kAlpha.ToBanjo() == LAYER_COMPOSITION_OPERATIONS_ALPHA);

}  // namespace display
