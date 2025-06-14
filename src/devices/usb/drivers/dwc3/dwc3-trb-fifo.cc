// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3-trb-fifo.h"

namespace dwc3 {

zx::result<> TrbFifo::Init(zx::bti& bti) {
  if (!buffer_) {
    zx_status_t status = dma_buffer::CreateBufferFactory()->CreateContiguous(
        bti, TrbFifo::kFifoSize, 12, true, &buffer_);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    first_ = static_cast<dwc3_trb_t*>(buffer_->virt());
    last_ = first_ + (TrbFifo::kFifoSize / sizeof(dwc3_trb_t)) - 1;

    // set up link TRB pointing back to the start of the fifo
    zx_paddr_t trb_phys = buffer_->phys();
    last_->ptr_low = (uint32_t)trb_phys;
    last_->ptr_high = (uint32_t)(trb_phys >> 32);
    last_->status = 0;
    last_->control = TRB_TRBCTL_LINK | TRB_HWO;
    Write(last_);
  }

  next_ = first_;
  current_ = next_;

  return zx::ok();
}

void TrbFifo::Release() {
  first_ = next_ = current_ = last_ = nullptr;
  buffer_.reset();
}

dwc3_trb_t TrbFifo::ReadCurrent() {
  CacheFlushInvalidate(buffer_.get(), (current_ - first_) * sizeof(dwc3_trb_t), sizeof(dwc3_trb_t));
  return *current_;
}

zx_paddr_t TrbFifo::Write(dwc3_trb_t* trb) {
  CacheFlush(buffer_.get(), (trb - first_) * sizeof(dwc3_trb_t), sizeof(dwc3_trb_t));
  return GetTrbPhys(trb);
}

dwc3_trb_t* TrbFifo::Advance(dwc3_trb_t*& trb) {
  dwc3_trb_t* cur = trb++;
  if (trb == last_) {
    trb = first_;
  }
  return cur;
}

}  // namespace dwc3
