// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_
#define SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_

#include <lib/dma-buffer/buffer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/result.h>

#include "src/devices/usb/drivers/dwc3/dwc3-types.h"

namespace dwc3 {

// A dma_buffer::ContiguousBuffer is cached, but leaves cache management to the user. These methods
// wrap zx_cache_flush with sensible boundary checking and validation.
zx_status_t CacheFlush(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset, size_t length);
zx_status_t CacheFlushInvalidate(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset,
                                 size_t length);

class TrbFifo {
 public:
  zx::result<> Init(zx::bti& bti);
  void Release();

  dwc3_trb_t ReadCurrent();
  zx_paddr_t Write(dwc3_trb_t* trb);
  dwc3_trb_t* AdvanceNext() { return Advance(next_); }
  void AdvanceCurrent() {
    if (current_ == next_) {
      FDF_LOG(ERROR, "Advancing current_ past next_. Invalid!");
      return;
    }
    Advance(current_);
  }

  void Clear() { current_ = next_; }

 private:
  static inline const uint32_t kFifoSize = zx_system_get_page_size();

  dwc3_trb_t* Advance(dwc3_trb_t*& trb);
  zx_paddr_t GetTrbPhys(dwc3_trb_t* trb) const {
    ZX_DEBUG_ASSERT((trb >= first_) && (trb <= last_));
    return buffer_->phys() + ((trb - first_) * sizeof(*trb));
  }

  std::unique_ptr<dma_buffer::ContiguousBuffer> buffer_;
  dwc3_trb_t* first_{nullptr};    // first TRB in the fifo
  dwc3_trb_t* next_{nullptr};     // next free TRB in the fifo
  dwc3_trb_t* current_{nullptr};  // TRB for currently pending transaction
  dwc3_trb_t* last_{nullptr};     // last TRB in the fifo (link TRB)
};

}  // namespace dwc3

#endif  // SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_
