// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3-trb-fifo.h"

#include "src/devices/usb/drivers/dwc3/dwc3.h"

namespace dwc3 {

zx_status_t Fifo::Init(zx::bti& bti) {
  if (buffer) {
    Reset();
    return ZX_OK;
  }

  zx_status_t status =
      dma_buffer::CreateBufferFactory()->CreateContiguous(bti, Fifo::kFifoSize, 12, true, &buffer);
  if (status != ZX_OK) {
    return status;
  }

  first = static_cast<dwc3_trb_t*>(buffer->virt());
  next = first;
  current = nullptr;
  last = first + (Fifo::kFifoSize / sizeof(dwc3_trb_t)) - 1;

  // set up link TRB pointing back to the start of the fifo
  dwc3_trb_t* trb = last;
  zx_paddr_t trb_phys = buffer->phys();
  trb->ptr_low = (uint32_t)trb_phys;
  trb->ptr_high = (uint32_t)(trb_phys >> 32);
  trb->status = 0;
  trb->control = TRB_TRBCTL_LINK | TRB_HWO;
  CacheFlush(buffer.get(), (trb - first) * sizeof(*trb), sizeof(*trb));

  return ZX_OK;
}

void Fifo::Release() {
  first = next = current = last = nullptr;
  buffer.reset();
}

void Dwc3::EpReadTrb(Endpoint& ep, Fifo& fifo, const dwc3_trb_t* src, dwc3_trb_t* dst) {
  if (src >= fifo.first && src < fifo.last) {
    CacheFlushInvalidate(fifo.buffer.get(), (src - fifo.first) * sizeof(*src), sizeof(*src));
    memcpy(dst, src, sizeof(*dst));
  } else {
    FDF_LOG(ERROR, "bad trb");
  }
}

}  // namespace dwc3
