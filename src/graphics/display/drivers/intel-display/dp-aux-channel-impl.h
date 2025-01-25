// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_DP_AUX_CHANNEL_IMPL_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_DP_AUX_CHANNEL_IMPL_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <threads.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstdint>

#include "src/graphics/display/drivers/intel-display/ddi-aux-channel.h"
#include "src/graphics/display/drivers/intel-display/dp-aux-channel.h"
#include "src/graphics/display/drivers/intel-display/hardware-common.h"

namespace intel_display {

class DpAuxChannelImpl : public DpAuxChannel, public ddk::I2cImplProtocol<DpAuxChannelImpl> {
 public:
  // `mmio_buffer` must outlive this instance.
  DpAuxChannelImpl(fdf::MmioBuffer* mmio_buffer, DdiId ddi_id, uint16_t device_id);

  zx_status_t I2cImplGetMaxTransferSize(uint64_t* out_size);
  zx_status_t I2cImplSetBitrate(uint32_t bitrate);
  zx_status_t I2cImplTransact(const i2c_impl_op_t* ops, size_t count);

  // `DpAuxChannel`:
  ddk::I2cImplProtocolClient i2c() final;
  bool DpcdRead(uint32_t addr, uint8_t* buf, size_t size) final;
  bool DpcdWrite(uint32_t addr, const uint8_t* buf, size_t size) final;

  // Exposed for configuration logging.
  DdiAuxChannel& aux_channel() { return aux_channel_; }

 private:
  DdiAuxChannel aux_channel_ __TA_GUARDED(lock_);
  mtx_t lock_;

  zx_status_t DpAuxRead(uint32_t dp_cmd, uint32_t addr, uint8_t* buf, size_t size)
      __TA_REQUIRES(lock_);
  zx_status_t DpAuxReadChunk(uint32_t dp_cmd, uint32_t addr, uint8_t* buf, uint32_t size_in,
                             size_t* size_out) __TA_REQUIRES(lock_);
  zx_status_t DpAuxWrite(uint32_t dp_cmd, uint32_t addr, const uint8_t* buf, size_t size)
      __TA_REQUIRES(lock_);

  zx::result<DdiAuxChannel::ReplyInfo> DoTransaction(const DdiAuxChannel::Request& request,
                                                     cpp20::span<uint8_t> reply_data_buffer)
      __TA_REQUIRES(lock_);
};

}  // namespace intel_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_DISPLAY_DP_AUX_CHANNEL_IMPL_H_
