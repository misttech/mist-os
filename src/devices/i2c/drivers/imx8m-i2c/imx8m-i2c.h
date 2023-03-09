// Copyright 2023 The Fuchsia Authors.All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_IMX8M_I2C_IMX8M_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_IMX8M_I2C_IMX8M_I2C_H_

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/async/cpp/irq.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/event.h>
#include <lib/zx/interrupt.h>

#include <memory>
#include <thread>

#include <ddktl/device.h>
#include <fbl/mutex.h>

#include "imx8m-i2c-regs.h"

namespace imx8m_i2c {

class Imx8mI2c;
class Imx8mI2cBus;

using DeviceType = ddk::Device<Imx8mI2c, ddk::Unbindable>;

class Imx8mI2c : public DeviceType, public ddk::I2cImplProtocol<Imx8mI2c, ddk::base_protocol> {
 public:
  Imx8mI2c(zx_device_t* parent, ddk::MmioBuffer mmio, zx::interrupt& irq)
      : DeviceType(parent), mmio_(std::move(mmio)), irq_(std::move(irq)) {}
  virtual ~Imx8mI2c() { Shutdown(); }

  static zx_status_t Create(void* ctx, zx_device_t* parent);
  void Shutdown();

  // Methods required by the ddk mixins.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  zx_status_t I2cImplGetMaxTransferSize(size_t* out_size);
  zx_status_t I2cImplSetBitrate(uint32_t bitrate);
  zx_status_t I2cImplTransact(const i2c_impl_op_t* ops, size_t count);

  zx_status_t Init();
  zx_status_t Transact(const i2c_impl_op_t* op_list, size_t count);
  zx_status_t SetBitRate(uint32_t bitrate);
  // visible and implemented for testing, not used on hardware.
  // Test coverage document mentions that when waiting on an asynchronous operation
  // in a test, it's best to wait indefinitely and let the test runner's overall
  // timeout expire, so below function is exposed to overwrite kDefaultTimeout with
  // zx::duration::infinite() during test.
  void SetTimeout(zx::duration timeout);

 private:
  zx_status_t Bind();

  zx_status_t HostInit();
  zx_status_t WaitForBusState(bool busy);
  zx_status_t PreStart();
  zx_status_t Start();
  zx_status_t RepeatedStart();
  zx_status_t Stop();
  zx_status_t WaitForTransactionComplete();
  zx_status_t IsAcked();
  zx_status_t Transmit(const i2c_impl_op_t& op);
  zx_status_t Receive(const i2c_impl_op_t& op, bool last_msg);
  zx_status_t SendAddress(const i2c_impl_op_t& op);
  void DumpRegs();
  void HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                 const zx_packet_interrupt_t* interrupt);

  ddk::MmioBuffer mmio_;
  async::IrqMethod<Imx8mI2c, &Imx8mI2c::HandleIrq> irq_handler_{this};
  zx::interrupt irq_;
  zx::event event_;
  zx::duration timeout_;

  static constexpr uint32_t kDefaultTimeout = ZX_MSEC(100);

  fbl::Mutex transact_lock_;  // used to serialize transactions

  static constexpr uint32_t kTransactionCompleteSignal = ZX_USER_SIGNAL_0;
  static constexpr uint32_t kTransactionError = ZX_USER_SIGNAL_1;

  static constexpr uint32_t kAddressRegOffset = 0x0;
  static constexpr uint32_t kFreqDivRegOffset = 0x4;
  static constexpr uint32_t kControlRegOffset = 0x8;
  static constexpr uint32_t kStatusRegOffset = 0xc;
  static constexpr uint32_t kDataRegOffset = 0x10;
};

}  // namespace imx8m_i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_IMX8M_I2C_IMX8M_I2C_H_
