// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/capture-image.h"

#include <lib/inspect/cpp/inspect.h>
#include <zircon/assert.h>

#include <cstdint>

#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display {

CaptureImage::CaptureImage(Controller* controller, uint64_t capture_image_handle,
                           inspect::Node* parent_node, uint32_t client_id)
    : capture_image_handle_(capture_image_handle), client_id_(client_id), controller_(controller) {
  ZX_DEBUG_ASSERT(controller_ != nullptr);

  InitializeInspect(parent_node);
}

CaptureImage::~CaptureImage() { controller_->ReleaseCaptureImage(capture_image_handle_); }

void CaptureImage::InitializeInspect(inspect::Node* parent_node) {
  if (!parent_node)
    return;
  node_ = parent_node->CreateChild(fbl::StringPrintf("capture-image-%p", this).c_str());
  node_.CreateUint("client_id", client_id_, &properties_);
}

}  // namespace display
