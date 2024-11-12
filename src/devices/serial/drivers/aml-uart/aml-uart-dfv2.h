// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV2_H_
#define SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV2_H_

#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/fidl.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>

#include "src/devices/serial/drivers/aml-uart/aml-uart.h"
#include "src/devices/serial/drivers/aml-uart/aml_uart_dfv2_config.h"

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
  zx_status_t GetPowerConfiguration(
      const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev);

  void OnReceivedMetadata(
      fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& metadata_result);

  void OnDeviceServerInitialized(zx::result<> device_server_init_result);

  void OnAddChildResult(
      fidl::WireUnownedResult<fuchsia_driver_framework::Node::AddChild>& add_child_result);

  void CompleteStart(zx::result<> result);

  std::optional<fdf::StartCompleter> start_completer_;
  fidl::WireClient<fuchsia_driver_compat::Device> compat_client_;
  fidl::WireClient<fuchsia_driver_framework::Node> parent_node_client_;
  fuchsia_hardware_serial::wire::SerialPortInfo serial_port_info_;
  std::optional<fdf::SynchronizedDispatcher> irq_dispatcher_;
  std::optional<fdf::SynchronizedDispatcher> timer_dispatcher_;
  std::optional<AmlUart> aml_uart_;
  std::optional<fdf::PrepareStopCompleter> prepare_stop_completer_;
  compat::AsyncInitializedDeviceServer device_server_;
  fdf::ServerBindingGroup<fuchsia_hardware_serialimpl::Device> serial_impl_bindings_;

  aml_uart_dfv2_config::Config driver_config_;
  std::optional<fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>> sag_;
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_AML_UART_AML_UART_DFV2_H_
