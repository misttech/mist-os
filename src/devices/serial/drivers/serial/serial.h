// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_
#define SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <zircon/types.h>

namespace serial {

class SerialDevice : public fdf::DriverBase,
                     public fidl::WireServer<fuchsia_hardware_serial::DeviceProxy>,
                     public fidl::WireServer<fuchsia_hardware_serial::Device> {
 public:
  SerialDevice(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("serial", std::move(start_args), std::move(dispatcher)),
        devfs_connector_(fit::bind_member<&SerialDevice::DevfsConnect>(this)) {}

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  zx_status_t Bind();
  zx_status_t Init();

  void GetChannel(GetChannelRequestView request, GetChannelCompleter::Sync& completer) override;

  void Read(ReadCompleter::Sync& completer) override;
  void Write(WriteRequestView request, WriteCompleter::Sync& completer) override;

 private:
  // Fidl protocol implementation.
  void GetClass(GetClassCompleter::Sync& completer) override;
  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override;

  zx_status_t Enable(bool enable);

  zx_status_t Bind(fidl::ServerEnd<fuchsia_hardware_serial::Device> server);
  void DevfsConnect(fidl::ServerEnd<fuchsia_hardware_serial::DeviceProxy> server);

  // The serial protocol of the device we are binding against.
  fdf::WireClient<fuchsia_hardware_serialimpl::Device> serial_;

  uint32_t serial_class_;
  driver_devfs::Connector<fuchsia_hardware_serial::DeviceProxy> devfs_connector_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> controller_;

  fidl::ServerBindingGroup<fuchsia_hardware_serial::DeviceProxy> proxy_bindings_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_serial::Device>> binding_;
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_
