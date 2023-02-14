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

uint32_t aml_i2c_get_bus_count(void* ctx);
uint32_t aml_i2c_get_bus_base(void* ctx);
zx_status_t aml_i2c_get_max_transfer_size(void* ctx, uint32_t bus_id, size_t* out_size);
zx_status_t aml_i2c_set_bitrate(void* ctx, uint32_t bus_id, uint32_t bitrate);
zx_status_t aml_i2c_transact(void* ctx, uint32_t bus_id, const i2c_impl_op_t* rws, size_t count);

class AmlI2c;
class AmlI2cDev;

using aml_i2c_t = AmlI2c;
using aml_i2c_dev_t = AmlI2cDev;

struct aml_i2c_regs_t;

class AmlI2cDev {
 public:
  AmlI2cDev() = default;
  ~AmlI2cDev() {
    irq_.destroy();
    if (irqthrd) {
      thrd_join(irqthrd, nullptr);
    }
  }

  zx_status_t Init(unsigned index, aml_i2c_delay_values delay, ddk::PDev pdev);

  // TODO(fxbug.dev/120969): Remove public members after C++ conversion.
  zx_handle_t irq;
  zx_handle_t event;
  MMIO_PTR volatile aml_i2c_regs_t* virt_regs;
  zx_duration_t timeout;
  thrd_t irqthrd{};

 private:
  zx::interrupt irq_;
  zx::event event_;
  std::optional<fdf::MmioBuffer> regs_iobuff_;
};

using DeviceType = ddk::Device<AmlI2c>;
class AmlI2c : public DeviceType, public ddk::I2cImplProtocol<AmlI2c, ddk::base_protocol> {
 public:
  static zx_status_t Bind(void* ctx, zx_device_t* parent);

  explicit AmlI2c(zx_device_t* parent, fbl::Array<AmlI2cDev> i2c_devs)
      : DeviceType(parent),
        i2c_devs(i2c_devs.data()),
        dev_count(static_cast<uint32_t>(i2c_devs.size())),
        i2c_devs_(std::move(i2c_devs)) {}

  void DdkRelease() { delete this; }

  uint32_t I2cImplGetBusBase() { return aml_i2c_get_bus_base(this); }
  uint32_t I2cImplGetBusCount() { return aml_i2c_get_bus_count(this); }
  zx_status_t I2cImplGetMaxTransferSize(uint32_t bus_id, uint64_t* out_size) {
    return aml_i2c_get_max_transfer_size(this, bus_id, out_size);
  }
  zx_status_t I2cImplSetBitrate(uint32_t bus_id, uint32_t bitrate) {
    return aml_i2c_set_bitrate(this, bus_id, bitrate);
  }
  zx_status_t I2cImplTransact(uint32_t bus_id, const i2c_impl_op_t* op_list, size_t op_count) {
    return aml_i2c_transact(this, bus_id, op_list, op_count);
  }

  // TODO(fxbug.dev/120969): Remove public members after C++ conversion.
  aml_i2c_dev_t* i2c_devs;
  uint32_t dev_count;

 private:
  const fbl::Array<AmlI2cDev> i2c_devs_;
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
