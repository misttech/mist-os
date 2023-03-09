// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_I2C_GMBUS_I2C_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_I2C_GMBUS_I2C_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/mmio/mmio-buffer.h>
#include <threads.h>
#include <zircon/assert.h>

#include "src/graphics/display/drivers/intel-i915/hardware-common.h"
#include "src/graphics/display/drivers/intel-i915/i2c/gmbus-gpio.h"

namespace i915 {

class GMBusI2c : public ddk::I2cImplProtocol<GMBusI2c> {
 public:
  GMBusI2c(DdiId ddi_id, registers::Platform platform, fdf::MmioBuffer* mmio_space);

  zx_status_t I2cImplGetMaxTransferSize(uint64_t* out_size);
  zx_status_t I2cImplSetBitrate(uint32_t bitrate);
  zx_status_t I2cImplTransact(const i2c_impl_op_t* ops, size_t count);

  ddk::I2cImplProtocolClient i2c() {
    const i2c_impl_protocol_t i2c{.ops = &i2c_impl_protocol_ops_, .ctx = this};
    return ddk::I2cImplProtocolClient(&i2c);
  }

 private:
  const std::optional<GMBusPinPair> gmbus_pin_pair_;
  const std::optional<GpioPort> gpio_port_;

  // The lock protects the registers this class writes to, not the whole
  // register io space.
  fdf::MmioBuffer* mmio_space_ __TA_GUARDED(lock_);
  mtx_t lock_;

  bool I2cFinish() __TA_REQUIRES(lock_);
  bool I2cWaitForHwReady() __TA_REQUIRES(lock_);
  bool I2cClearNack() __TA_REQUIRES(lock_);
  bool SetDdcSegment(uint8_t block_num) __TA_REQUIRES(lock_);
  bool GMBusRead(uint8_t addr, uint8_t* buf, uint8_t size) __TA_REQUIRES(lock_);
  bool GMBusWrite(uint8_t addr, const uint8_t* buf, uint8_t size) __TA_REQUIRES(lock_);
};

}  // namespace i915

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_I2C_GMBUS_I2C_H_
