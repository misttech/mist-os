// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_
#define SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_

#include "src/devices/usb/drivers/dwc3/dwc3-fifo.h"
#include "src/devices/usb/drivers/dwc3/dwc3-types.h"

namespace dwc3 {

class TrbFifo : public Fifo<dwc3_trb_t> {
 public:
  zx::result<> Init(zx::bti& bti) override {
    bool needs_init = !buffer_;
    auto result = Fifo::Init(bti);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to init FIFO %s", result.status_string());
      return result.take_error();
    }

    if (needs_init) {
      // set up link TRB pointing back to the start of the fifo
      zx_paddr_t trb_phys = Fifo::GetPhys(first_);
      last_--;
      last_->ptr_low = (uint32_t)trb_phys;
      last_->ptr_high = (uint32_t)(trb_phys >> 32);
      last_->status = 0;
      last_->control = TRB_TRBCTL_LINK | TRB_HWO;
      CacheFlush(buffer_.get(), (last_ - first_) * sizeof(dwc3_trb_t), sizeof(dwc3_trb_t));
    }
    return zx::ok();
  }

  dwc3_trb_t Read() {
    std::vector<dwc3_trb_t> trbs = Fifo::Read(read_, 1);
    ZX_DEBUG_ASSERT(trbs.size() == 1);
    return trbs[0];
  }
  dwc3_trb_t* AdvanceWrite() { return Fifo::Advance(write_); }
  void AdvanceRead() {
    if (read_ == write_) {
      FDF_LOG(ERROR, "Advancing read_ past write_. Invalid!");
      return;
    }
    Fifo::Advance(read_);
  }
};

}  // namespace dwc3

#endif  // SRC_DEVICES_USB_DRIVERS_DWC3_DWC3_TRB_FIFO_H_
