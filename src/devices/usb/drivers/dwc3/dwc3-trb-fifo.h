// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_
#define SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_

#include <lib/dma-buffer/buffer.h>

#include "src/devices/usb/drivers/dwc3/dwc3-types.h"

namespace dwc3 {

struct Fifo {
  static inline const uint32_t kFifoSize = zx_system_get_page_size();

  zx_status_t Init(zx::bti& bti);
  void Release();
  void Reset() {
    current = nullptr;
    next = first;
  }

  zx_paddr_t GetTrbPhys(dwc3_trb_t* trb) const {
    ZX_DEBUG_ASSERT((trb >= first) && (trb <= last));
    return buffer->phys() + ((trb - first) * sizeof(*trb));
  }

  std::unique_ptr<dma_buffer::ContiguousBuffer> buffer;
  dwc3_trb_t* first{nullptr};    // first TRB in the fifo
  dwc3_trb_t* next{nullptr};     // next free TRB in the fifo
  dwc3_trb_t* current{nullptr};  // TRB for currently pending transaction
  dwc3_trb_t* last{nullptr};     // last TRB in the fifo (link TRB)
};

}  // namespace dwc3

#endif  // SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_
