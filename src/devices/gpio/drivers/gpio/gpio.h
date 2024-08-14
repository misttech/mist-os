// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_
#define SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/driver/wire.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <stdio.h>

#include <optional>
#include <string>
#include <string_view>

#include <ddk/metadata/gpio.h>

namespace gpio {

class GpioDevice : public fidl::WireServer<fuchsia_hardware_gpio::Gpio> {
 public:
  GpioDevice(fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio, uint32_t pin,
             uint32_t controller_id, std::string_view name)
      : fidl_dispatcher_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        pin_(pin),
        controller_id_(controller_id),
        name_(name),
        gpio_(std::move(gpio)),
        devfs_connector_(fit::bind_member<&GpioDevice::DevfsConnect>(this)) {}

  zx::result<> AddServices(const std::shared_ptr<fdf::Namespace>& incoming,
                           const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                           const std::optional<std::string>& node_name);

  zx::result<> AddDevice(fidl::UnownedClientEnd<fuchsia_driver_framework::Node> root_node,
                         fdf::Logger& logger);

 private:
  void DevfsConnect(fidl::ServerEnd<fuchsia_hardware_gpio::Gpio> server);

  void GetPin(GetPinCompleter::Sync& completer) override;
  void GetName(GetNameCompleter::Sync& completer) override;
  void ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) override;
  void ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) override;
  void Read(ReadCompleter::Sync& completer) override;
  void Write(WriteRequestView request, WriteCompleter::Sync& completer) override;
  void SetDriveStrength(SetDriveStrengthRequestView request,
                        SetDriveStrengthCompleter::Sync& completer) override;
  void GetDriveStrength(GetDriveStrengthCompleter::Sync& completer) override;
  void GetInterrupt(GetInterruptRequestView request,
                    GetInterruptCompleter::Sync& completer) override;
  void ConfigureInterrupt(fuchsia_hardware_gpio::wire::GpioConfigureInterruptRequest* request,
                          ConfigureInterruptCompleter::Sync& completer) override;
  void ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) override;
  void SetAltFunction(SetAltFunctionRequestView request,
                      SetAltFunctionCompleter::Sync& completer) override;

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_gpio::Gpio> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  std::string pin_name() const {
    char name[20];
    snprintf(name, sizeof(name), "gpio-%u", pin_);
    return name;
  }

  async_dispatcher_t* const fidl_dispatcher_;
  const uint32_t pin_;
  const uint32_t controller_id_;
  const std::string name_;

  fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio_;
  fidl::ServerBindingGroup<fuchsia_hardware_gpio::Gpio> bindings_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fuchsia_hardware_gpio::Gpio> devfs_connector_;
};

class GpioInitDevice {
 public:
  static std::unique_ptr<GpioInitDevice> Create(
      const std::shared_ptr<fdf::Namespace>& incoming,
      fidl::UnownedClientEnd<fuchsia_driver_framework::Node> node, fdf::Logger& logger,
      uint32_t controller_id, fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl>& gpio);

 private:
  static zx_status_t ConfigureGpios(
      const fuchsia_hardware_gpioimpl::wire::InitMetadata& metadata,
      fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl>& gpio);

  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
};

class GpioRootDevice : public fdf::DriverBase {
 public:
  GpioRootDevice(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("gpio", std::move(start_args), std::move(dispatcher)) {}

  void Start(fdf::StartCompleter completer) override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

 private:
  // GpioDevice instances live on fidl_dispatcher_ so that they can run with a certain scheduler
  // role if one is provided. This conflicts with the requirement on our outgoing directory, which
  // serves the GPIO service and lives on the driver dispatcher. To handle this, initializing
  // GpioDevice instances uses a three-step process:
  //     1. Create the GpioDevice instances on fidl_dispatcher_ so that their thread-unsafe members
  //        (fdf::WireClient, fidl::ServerBindingGroup) live there.
  //     2. Add services to outgoing() on the driver dispatcher. Connections will be made using this
  //        dispatcher, so service handlers should post tasks to fidl_dispatcher_ if needed.
  //     3. Add GpioDevice nodes on fidl_dispatcher_.

  // Must be run on the FIDL dispatcher.
  void CreatePinDevices(uint32_t controller_id, const std::vector<gpio_pin_t>& pins,
                        fdf::StartCompleter completer);

  // Must be run on the driver dispatcher.
  void ServePinDevices(fdf::StartCompleter completer);

  // Must be run on the FIDL dispatcher.
  void AddPinDevices(fdf::StartCompleter completer);

  void ClientTeardownHandler();

  fdf::UnownedDispatcher fidl_dispatcher() const {
    return fidl_dispatcher_ ? fdf::UnownedDispatcher(fidl_dispatcher_->get())
                            : fdf::Dispatcher::GetCurrent();
  }

  std::optional<fdf::PrepareStopCompleter> stop_completer_;
  std::optional<fdf::SynchronizedDispatcher> fidl_dispatcher_;
  fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio_;
  std::vector<std::unique_ptr<GpioDevice>> children_;
  std::unique_ptr<GpioInitDevice> init_device_;

  fdf::OwnedChildNode node_;
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_
