// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_EVENT_FIFO_H_
#define SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_EVENT_FIFO_H_

#include <lib/dma-buffer/buffer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>

namespace dwc3 {

// A dma_buffer::ContiguousBuffer is cached, but leaves cache management to the user. These methods
// wrap zx_cache_flush with sensible boundary checking and validation.
zx_status_t CacheFlush(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset, size_t length);
zx_status_t CacheFlushInvalidate(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset,
                                 size_t length);

class EventFifo {
 public:
  zx::result<> Init(zx::bti& bti);
  void Release() { buffer_.reset(); }

  void Read(size_t count);
  uint32_t Advance();

  zx_paddr_t GetPhys() const { return buffer_->phys(); }
  static inline const uint32_t kEventBufferSize = zx_system_get_page_size();

 private:
  std::unique_ptr<dma_buffer::ContiguousBuffer> buffer_;
  uint32_t* first_{nullptr};    // first event in the fifo
  uint32_t* current_{nullptr};  // next event in the fifo
  uint32_t* last_{nullptr};     // last event in the fifo
};

}  // namespace dwc3

#endif  // SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_EVENT_FIFO_H_
