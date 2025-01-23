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

#include <atomic>
#include <utility>

#include <fbl/ref_ptr.h>
#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/fence.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"
#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

namespace display_coordinator {

Image::Image(Controller* controller, const display::ImageMetadata& metadata,
             display::DriverImageId driver_id, inspect::Node* parent_node, ClientId client_id)
    : driver_id_(driver_id), metadata_(metadata), controller_(*controller), client_id_(client_id) {
  ZX_DEBUG_ASSERT(controller);
  ZX_DEBUG_ASSERT(metadata.tiling_type() != display::ImageTilingType::kCapture);
  InitializeInspect(parent_node);
}
Image::~Image() {
  ZX_ASSERT(!std::atomic_load(&in_use_));
  ZX_ASSERT(!InDoublyLinkedList());
  controller_.ReleaseImage(driver_id_);
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

void Image::StartPresent() {
  TRACE_DURATION("gfx", "Image::StartPresent", "id", id.value());
  TRACE_FLOW_BEGIN("gfx", "present_image", id.value());

  presenting_ = true;
  presenting_property_.Set(true);
}

void Image::EarlyRetire() {
  // A client may reuse an image as soon as retire_fence_ fires. Set in_use_ first.
  std::atomic_store(&in_use_, false);
}

void Image::StartRetire() {
  if (!presenting_) {
    std::atomic_store(&in_use_, false);
  } else {
    retiring_ = true;
    retiring_property_.Set(true);
  }
}

void Image::OnRetire() {
  presenting_ = false;
  presenting_property_.Set(false);

  if (retiring_) {
    std::atomic_store(&in_use_, false);
    retiring_ = false;
    retiring_property_.Set(false);
  }
}

void Image::DiscardAcquire() { std::atomic_store(&in_use_, false); }

bool Image::Acquire() { return !std::atomic_exchange(&in_use_, true); }

}  // namespace display_coordinator
