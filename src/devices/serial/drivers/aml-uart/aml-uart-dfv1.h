// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV1_H_
#define SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV1_H_

#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/fidl.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include <ddktl/device.h>

#include "src/devices/serial/drivers/aml-uart/aml-uart.h"

namespace serial {

class AmlUartV1;
using DeviceType = ddk::Device<AmlUartV1, ddk::Unbindable>;

class AmlUartV1 : public DeviceType {
 public:
  // Spawns device node.
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  explicit AmlUartV1(zx_device_t* parent)
      : DeviceType(parent),
        outgoing_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())) {}

  // Device protocol implementation.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  zx_status_t Init(ddk::PDevFidl pdev,
                   const fuchsia_hardware_serial::wire::SerialPortInfo& serial_port_info,
                   fdf::MmioBuffer mmio);

  // Used by the unit test to access the device.
  AmlUart& aml_uart_for_testing() { return aml_uart_.value(); }

 private:
  std::optional<fdf::SynchronizedDispatcher> irq_dispatcher_;
  std::optional<AmlUart> aml_uart_;
  std::optional<ddk::UnbindTxn> unbind_txn_;
  fdf::OutgoingDirectory outgoing_;
  fdf::ServerBindingGroup<fuchsia_hardware_serialimpl::Device> serial_impl_bindings_;
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV1_H_
