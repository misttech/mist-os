// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV1_H_
#define SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV1_H_

#include <ddktl/device.h>

#include "src/devices/serial/drivers/aml-uart/aml-uart.h"

namespace serial {

class AmlUartV1;
using DeviceType = ddk::Device<AmlUartV1, ddk::GetProtocolable, ddk::Unbindable>;

class AmlUartV1 : public DeviceType {
 public:
  // Spawns device node.
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  explicit AmlUartV1(zx_device_t* parent) : DeviceType(parent) {}

  // Device protocol implementation.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  // ddk::GetProtocolable
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

  zx_status_t Init(ddk::PDevFidl pdev, const serial_port_info_t& serial_port_info,
                   fdf::MmioBuffer mmio);

  // Used by the unit test to access the device.
  AmlUart& aml_uart_for_testing() { return aml_uart_.value(); }

 private:
  std::optional<fdf::SynchronizedDispatcher> irq_dispatcher_;
  std::optional<AmlUart> aml_uart_;
  std::optional<ddk::UnbindTxn> unbind_txn_;
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV1_H_
