// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/capture-image.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/inspect/cpp/inspect.h>

#include <cstdint>

#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display {

CaptureImage::CaptureImage(Controller* controller, const image_t& info, inspect::Node* parent_node,
                           uint32_t client_id)
    : info_(info), controller_(controller), client_id_(client_id) {
  InitializeInspect(parent_node);
}

CaptureImage::~CaptureImage() { controller_->ReleaseCaptureImage(info_.handle); }

void CaptureImage::InitializeInspect(inspect::Node* parent_node) {
  if (!parent_node)
    return;
  node_ = parent_node->CreateChild(fbl::StringPrintf("capture-image-%p", this).c_str());
  node_.CreateUint("width", info_.width, &properties_);
  node_.CreateUint("height", info_.height, &properties_);
  node_.CreateUint("type", info_.type, &properties_);
}

}  // namespace display
