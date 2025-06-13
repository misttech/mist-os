// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3-event-fifo.h"

namespace dwc3 {

zx::result<> EventFifo::Init(zx::bti& bti) {
  if (!buffer_) {
    // Strictly speaking, we should not need RW access to this buffer.
    // Unfortunately, attempting to writeback and invalidate the cache before
    // reading anything from the buffer produces a page fault right if this buffer
    // is mapped read only, so for now, we keep the buffer mapped RW.
    zx_status_t status = dma_buffer::CreateBufferFactory()->CreateContiguous(bti, kEventBufferSize,
                                                                             12, true, &buffer_);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "dma_buffer init fails: %s", zx_status_get_string(status));
      return zx::error(status);
    }
    CacheFlushInvalidate(buffer_.get(), 0, kEventBufferSize);

    first_ = static_cast<uint32_t*>(buffer_->virt());
    last_ = first_ + (kEventBufferSize / sizeof(uint32_t));
  }

  current_ = first_;

  return zx::ok();
}

void EventFifo::Read(size_t count) {
  // invalidate cache so we can read fresh events
  const zx_off_t offset = (current_ - first_) * sizeof(uint32_t);
  const size_t todo = std::min<size_t>(last_ - current_, count);
  CacheFlushInvalidate(buffer_.get(), offset, todo * sizeof(uint32_t));
  if (count > todo) {
    CacheFlushInvalidate(buffer_.get(), 0, (count - todo) * sizeof(uint32_t));
  }
}

uint32_t EventFifo::Advance() {
  uint32_t* cur = current_++;
  if (current_ == last_) {
    current_ = first_;
  }
  return *cur;
}

}  // namespace dwc3
