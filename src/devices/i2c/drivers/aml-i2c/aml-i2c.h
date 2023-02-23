// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/device-protocol/pdev.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/time.h>
#include <threads.h>

#include <vector>

#include <ddktl/device.h>
#include <soc/aml-common/aml-i2c.h>

namespace aml_i2c {

class AmlI2cDev {
 public:
  AmlI2cDev(zx::interrupt irq, zx::event event, fdf::MmioBuffer regs_iobuff)
      : irq_(std::move(irq)), event_(std::move(event)), regs_iobuff_(std::move(regs_iobuff)) {}

  ~AmlI2cDev() {
    irq_.destroy();
    if (irqthrd_) {
      thrd_join(irqthrd_, nullptr);
    }
  }

  // Move constructor must exist to be able to use std::vector, but should not be called.
  AmlI2cDev(AmlI2cDev&& other) noexcept
      : irq_(std::move(other.irq_)),
        event_(std::move(other.event_)),
        regs_iobuff_(std::move(other.regs_iobuff_)) {
    ZX_DEBUG_ASSERT_MSG(false, "Move constructor called");
  }
  AmlI2cDev& operator=(AmlI2cDev&& other) = delete;

  zx_status_t Transact(const i2c_impl_op_t* rws, size_t count) const;

  void StartIrqThread();

 private:
  friend class AmlI2cTest;

  void SetTargetAddr(uint16_t addr) const;
  void StartXfer() const;
  zx_status_t WaitTransferComplete() const;

  zx_status_t Read(uint8_t* buff, uint32_t len, bool stop) const;
  zx_status_t Write(const uint8_t* buff, uint32_t len, bool stop) const;

  int IrqThread() const;

  zx::interrupt irq_;
  zx::event event_;
  fdf::MmioBuffer regs_iobuff_;
  zx::duration timeout_ = zx::sec(1);
  thrd_t irqthrd_{};
};

class AmlI2c;
using DeviceType = ddk::Device<AmlI2c>;

class AmlI2c : public DeviceType, public ddk::I2cImplProtocol<AmlI2c, ddk::base_protocol> {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  explicit AmlI2c(zx_device_t* parent) : DeviceType(parent) {}

  void DdkRelease() { delete this; }

  uint32_t I2cImplGetBusBase();
  uint32_t I2cImplGetBusCount();
  zx_status_t I2cImplGetMaxTransferSize(uint32_t bus_id, uint64_t* out_size);
  zx_status_t I2cImplSetBitrate(uint32_t bus_id, uint32_t bitrate);
  zx_status_t I2cImplTransact(uint32_t bus_id, const i2c_impl_op_t* rws, size_t count);

 private:
  friend class AmlI2cTest;

  zx_status_t InitDevice(uint32_t index, aml_i2c_delay_values delay, ddk::PDev pdev);

  std::vector<AmlI2cDev> i2c_devs_;
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
