// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_

#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/time.h>
#include <threads.h>

#include "soc/aml-common/aml-i2c.h"

namespace aml_i2c {

class AmlI2c : public fdf::WireServer<fuchsia_hardware_i2cimpl::Device> {
 public:
  // Create an `AmlI2c` object and return a pointer to it. The return type is a
  // pointer to `AmlI2c` and not just `AmlI2c` because `AmlI2c` is not copyable.
  static zx::result<std::unique_ptr<AmlI2c>> Create(ddk::PDevFidl& pdev,
                                                    const aml_i2c_delay_values& delay);

  AmlI2c(zx::interrupt irq, zx::event event, fdf::MmioBuffer regs_iobuff)
      : irq_(std::move(irq)), event_(std::move(event)), regs_iobuff_(std::move(regs_iobuff)) {
    StartIrqThread();
  }

  ~AmlI2c() {
    irq_.destroy();
    if (irqthrd_) {
      thrd_join(irqthrd_, nullptr);
    }
  }

  // I2cImpl protocol implementation
  void GetMaxTransferSize(fdf::Arena& arena, GetMaxTransferSizeCompleter::Sync& completer) override;
  void SetBitrate(SetBitrateRequestView request, fdf::Arena& arena,
                  SetBitrateCompleter::Sync& completer) override;
  void Transact(TransactRequestView request, fdf::Arena& arena,
                TransactCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_i2cimpl::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  thrd_t irqthrd() const { return irqthrd_; }

  void SetTimeout(zx::duration timeout) { timeout_ = timeout; }

  fuchsia_hardware_i2cimpl::Service::InstanceHandler GetI2cImplInstanceHandler(
      fdf_dispatcher_t* dispatcher);

 private:
  static zx_status_t SetClockDelay(const aml_i2c_delay_values& delay,
                                   const fdf::MmioBuffer& regs_iobuff);

  void SetTargetAddr(uint16_t addr) const;
  void StartXfer() const;
  zx_status_t WaitTransferComplete() const;

  zx_status_t Read(cpp20::span<uint8_t> dst, bool stop) const;
  zx_status_t Write(cpp20::span<uint8_t> src, bool stop) const;

  void StartIrqThread();
  int IrqThread() const;

  const zx::interrupt irq_;
  const zx::event event_;
  const fdf::MmioBuffer regs_iobuff_;
  zx::duration timeout_ = zx::sec(1);
  thrd_t irqthrd_{};
  fdf::ServerBindingGroup<fuchsia_hardware_i2cimpl::Device> i2cimpl_bindings_;
};

}  // namespace aml_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_AML_I2C_AML_I2C_H_
