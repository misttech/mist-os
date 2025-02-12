// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/image.h"

#include <lib/driver/logging/cpp/logger.h>
#include <lib/trace/event.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

namespace display_coordinator {

Image::Image(Controller* controller, const display::ImageMetadata& metadata,
             display::DriverImageId driver_id, inspect::Node* parent_node, ClientId client_id)
    : driver_id_(driver_id), metadata_(metadata), controller_(*controller), client_id_(client_id) {
  ZX_DEBUG_ASSERT(controller);
  ZX_DEBUG_ASSERT(driver_id != display::kInvalidDriverImageId);
  ZX_DEBUG_ASSERT(client_id != kInvalidClientId);
  ZX_DEBUG_ASSERT(metadata.tiling_type() != display::ImageTilingType::kCapture);
  InitializeInspect(parent_node);
}
Image::~Image() {
  ZX_ASSERT(!InDoublyLinkedList());
  if (!disposed_) {
    controller_.ReleaseImage(driver_id_);
  }
}

void Image::InitializeInspect(inspect::Node* parent_node) {
  if (!parent_node)
    return;
  node_ = parent_node->CreateChild(fbl::StringPrintf("image-%p", this).c_str());
  node_.CreateInt("width", metadata_.width(), &properties_);
  node_.CreateInt("height", metadata_.height(), &properties_);
  node_.CreateUint("tiling_type", metadata_.tiling_type().ValueForLogging(), &properties_);
  presenting_property_ = node_.CreateBool("presenting", false);
  retiring_property_ = node_.CreateBool("retiring", false);
}

fbl::Mutex* Image::mtx() const { return controller_.mtx(); }

bool Image::InDoublyLinkedList() const { return doubly_linked_list_node_state_.InContainer(); }

fbl::RefPtr<Image> Image::RemoveFromDoublyLinkedList() {
  return doubly_linked_list_node_state_.RemoveFromContainer<DefaultDoublyLinkedListTraits>();
}

}  // namespace display_coordinator
