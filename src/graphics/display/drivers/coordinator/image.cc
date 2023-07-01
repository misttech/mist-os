// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/image.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/zx/handle.h>
#include <zircon/assert.h>

#include <atomic>
#include <utility>

#include <fbl/intrusive_container_utils.h>
#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display {

Image::Image(Controller* controller, const image_t& info, zx::vmo vmo, inspect::Node* parent_node,
             ClientId client_id)
    : info_(info), controller_(controller), client_id_(client_id), vmo_(std::move(vmo)) {
  ZX_DEBUG_ASSERT(info.type != IMAGE_TYPE_CAPTURE);
  InitializeInspect(parent_node);
}
Image::~Image() {
  ZX_ASSERT(!std::atomic_load(&in_use_));
  ZX_ASSERT(!InDoublyLinkedList());
  controller_->ReleaseImage(this);
}

void Image::InitializeInspect(inspect::Node* parent_node) {
  if (!parent_node)
    return;
  node_ = parent_node->CreateChild(fbl::StringPrintf("image-%p", this).c_str());
  node_.CreateUint("width", info_.width, &properties_);
  node_.CreateUint("height", info_.height, &properties_);
  node_.CreateUint("type", info_.type, &properties_);
  presenting_property_ = node_.CreateBool("presenting", false);
  retiring_property_ = node_.CreateBool("retiring", false);
}

mtx_t* Image::mtx() const { return controller_->mtx(); }

bool Image::InDoublyLinkedList() const { return doubly_linked_list_node_state_.InContainer(); }

fbl::RefPtr<Image> Image::RemoveFromDoublyLinkedList() {
  return doubly_linked_list_node_state_.RemoveFromContainer<DefaultDoublyLinkedListTraits>();
}

void Image::PrepareFences(fbl::RefPtr<FenceReference>&& wait,
                          fbl::RefPtr<FenceReference>&& retire) {
  wait_fence_ = std::move(wait);
  retire_fence_ = std::move(retire);

  if (wait_fence_) {
    zx_status_t status = wait_fence_->StartReadyWait();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to start waiting %d", status);
      // Mark the image as ready. Displaying garbage is better than hanging or crashing.
      wait_fence_ = nullptr;
    }
  }
}

bool Image::OnFenceReady(FenceReference* fence) {
  if (wait_fence_.get() == fence) {
    wait_fence_ = nullptr;
  }
  return wait_fence_ == nullptr;
}

void Image::StartPresent() {
  ZX_DEBUG_ASSERT(wait_fence_ == nullptr);
  ZX_DEBUG_ASSERT(mtx_trylock(mtx()) == thrd_busy);
  TRACE_DURATION("gfx", "Image::StartPresent", "id", id.value());
  TRACE_FLOW_BEGIN("gfx", "present_image", id.value());

  presenting_ = true;
  presenting_property_.Set(true);
}

void Image::EarlyRetire() {
  // A client may re-use an image as soon as retire_fence_ fires. Set in_use_ first.
  std::atomic_store(&in_use_, false);
  if (wait_fence_) {
    wait_fence_->SetImmediateRelease(std::move(retire_fence_));
    wait_fence_ = nullptr;
  } else if (retire_fence_) {
    retire_fence_->Signal();
    retire_fence_ = nullptr;
  }
}

void Image::RetireWithFence(fbl::RefPtr<FenceReference>&& fence) {
  // Retire and acquire are not synchronized, so set in_use_ before signaling so
  // that the image can be reused as soon as the event is signaled. We don't have
  // to worry about the armed signal fence being overwritten on reuse since it is
  // on set in StartRetire, which is called under the same lock as OnRetire.
  std::atomic_store(&in_use_, false);
  if (fence) {
    fence->Signal();
  }
}

void Image::StartRetire() {
  ZX_DEBUG_ASSERT(wait_fence_ == nullptr);
  ZX_DEBUG_ASSERT(mtx_trylock(mtx()) == thrd_busy);

  if (!presenting_) {
    RetireWithFence(std::move(retire_fence_));
  } else {
    retiring_ = true;
    retiring_property_.Set(true);
    armed_retire_fence_ = std::move(retire_fence_);
  }
}

void Image::OnRetire() {
  ZX_DEBUG_ASSERT(mtx_trylock(mtx()) == thrd_busy);

  presenting_ = false;
  presenting_property_.Set(false);

  if (retiring_) {
    RetireWithFence(std::move(armed_retire_fence_));
    retiring_ = false;
    retiring_property_.Set(false);
  }
}

void Image::DiscardAcquire() {
  ZX_DEBUG_ASSERT(wait_fence_ == nullptr);

  std::atomic_store(&in_use_, false);
}

bool Image::Acquire() { return !std::atomic_exchange(&in_use_, true); }

void Image::ResetFences() {
  if (wait_fence_) {
    wait_fence_->ResetReadyWait();
  }

  wait_fence_ = nullptr;
  armed_retire_fence_ = nullptr;
  retire_fence_ = nullptr;
}

bool Image::HasSameDisplayPropertiesAsLayer(const image_t& layer_config) const {
  // TODO(fxbug.dev/126156): Currently this function only compares size and
  // usage type between current Image and a given Layer's accepted
  // configuration.
  //
  // We don't set the pixel format a Layer can accept, and we don't compare the
  // Image's pixel format against any accepted pixel format, assuming that all
  // image buffers allocated by sysmem can always be used for scanout in any
  // Layer. Currently, this assumption works for all our existing display engine
  // drivers. However, switching pixel formats in a Layer may cause performance
  // reduction, or might be not supported by new display engines / new display
  // formats.
  //
  // We should figure out a mechanism to indicate pixel format / modifiers
  // support for a Layer's image configuration (as opposed of using image_t),
  // and compare this Image's sysmem buffer collection information against the
  // Layer's format support.
  return info_.width == layer_config.width && info_.height == layer_config.height &&
         info_.type == layer_config.type;
}

}  // namespace display
