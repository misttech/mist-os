// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV2_H_
#define SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV2_H_

#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>

#include "src/devices/serial/drivers/aml-uart/aml-uart.h"

namespace serial {

class AmlUartV2 : public fdf::DriverBase {
 public:
  explicit AmlUartV2(fdf::DriverStartArgs start_args,
                     fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  void Start(fdf::StartCompleter completer) override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Used by the unit test to access the device.
  AmlUart& aml_uart_for_testing();

 private:
  void OnReceivedMetadata(
      fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& metadata_result);

  void OnDeviceServerInitialized(zx::result<> device_server_init_result);

  void OnAddChildResult(
      fidl::WireUnownedResult<fuchsia_driver_framework::Node::AddChild>& add_child_result);

  void CompleteStart(zx::result<> result);

  compat::DeviceServer::BanjoConfig GetBanjoConfig();

  std::optional<fdf::StartCompleter> start_completer_;
  fidl::WireClient<fuchsia_driver_compat::Device> compat_client_;
  fidl::WireClient<fuchsia_driver_framework::Node> parent_node_client_;
  serial_port_info_t serial_port_info_;
  std::optional<AmlUart> aml_uart_;
  compat::DeviceServer device_server_;
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV2_H_
