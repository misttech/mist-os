// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/device-protocol/pdev.h>
#include <lib/mmio-ptr/mmio-ptr.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/time.h>
#include <threads.h>

#include <optional>

#include <ddktl/device.h>
#include <fbl/array.h>
#include <soc/aml-common/aml-i2c.h>

namespace aml_i2c {

struct aml_i2c_regs_t;

class AmlI2cDev {
 public:
  AmlI2cDev() = default;
  ~AmlI2cDev() {
    irq_.destroy();
    if (irqthrd_) {
      thrd_join(irqthrd_, nullptr);
    }
  }

  zx_status_t Init(unsigned index, aml_i2c_delay_values delay, ddk::PDev pdev);

  zx_status_t Transact(const i2c_impl_op_t* rws, size_t count) const;

  // TODO(fxbug.dev/120969): Remove public members after C++ conversion.
  MMIO_PTR volatile aml_i2c_regs_t* virt_regs;

 private:
  friend class AmlI2cTest;

  zx_status_t SetTargetAddr(uint16_t addr) const;
  zx_status_t StartXfer() const;
  zx_status_t WaitEvent(uint32_t sig_mask) const;

  zx_status_t Read(uint8_t* buff, uint32_t len, bool stop) const;
  zx_status_t Write(const uint8_t* buff, uint32_t len, bool stop) const;

  int IrqThread() const;

  zx::interrupt irq_;
  zx::event event_;
  std::optional<fdf::MmioBuffer> regs_iobuff_;
  zx::duration timeout_ = zx::sec(1);
  thrd_t irqthrd_{};
};

class AmlI2c;
using DeviceType = ddk::Device<AmlI2c>;

class AmlI2c : public DeviceType, public ddk::I2cImplProtocol<AmlI2c, ddk::base_protocol> {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  AmlI2c(zx_device_t* parent, fbl::Array<AmlI2cDev> i2c_devs)
      : DeviceType(parent), i2c_devs_(std::move(i2c_devs)) {}

  void DdkRelease() { delete this; }

  uint32_t I2cImplGetBusBase();
  uint32_t I2cImplGetBusCount();
  zx_status_t I2cImplGetMaxTransferSize(uint32_t bus_id, uint64_t* out_size);
  zx_status_t I2cImplSetBitrate(uint32_t bus_id, uint32_t bitrate);
  zx_status_t I2cImplTransact(uint32_t bus_id, const i2c_impl_op_t* rws, size_t count);

 private:
  friend class AmlI2cTest;

  const fbl::Array<AmlI2cDev> i2c_devs_;
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
