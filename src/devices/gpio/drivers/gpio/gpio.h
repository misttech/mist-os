// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_
#define SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/driver/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>

#include <optional>
#include <string_view>

#include <ddk/metadata/gpio.h>
#include <ddktl/device.h>
#include <ddktl/fidl.h>

namespace gpio {

class GpioDevice;
using GpioDeviceType =
    ddk::Device<GpioDevice, ddk::Messageable<fuchsia_hardware_gpio::Gpio>::Mixin, ddk::Unbindable>;

class GpioRootDevice;
using GpioRootDeviceType = ddk::Device<GpioRootDevice, ddk::Unbindable>;

class GpioRootDevice : public GpioRootDeviceType {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

 private:
  explicit GpioRootDevice(zx_device_t* parent)
      : GpioRootDeviceType(parent), driver_dispatcher_(fdf::Dispatcher::GetCurrent()) {}

  zx_status_t AddPinDevices(uint32_t controller_id, const std::vector<gpio_pin_t>& pins);
  void DispatcherShutdownHandler(fdf_dispatcher_t* dispatcher);

  const fdf::UnownedDispatcher driver_dispatcher_;
  std::optional<fdf::SynchronizedDispatcher> fidl_dispatcher_;
  std::optional<ddk::UnbindTxn> unbind_txn_;
};

// GpioDevice instances live on fidl_dispatcher_, which is either a dispatcher owned by the
// GpioRootDevice, or the default driver dispatcher. Driver hooks (DdkUnbind and DdkRelease) always
// run on the default driver dispatcher.
class GpioDevice : public GpioDeviceType {
 public:
  GpioDevice(zx_device_t* parent, fdf::UnownedDispatcher fidl_dispatcher,
             fdf::ClientEnd<fuchsia_hardware_gpioimpl::GpioImpl> gpio, uint32_t pin,
             uint32_t controller_id, std::string_view name)
      : GpioDeviceType(parent),
        fidl_dispatcher_(std::move(fidl_dispatcher)),
        pin_(pin),
        controller_id_(controller_id),
        name_(name),
        gpio_(std::in_place, std::move(gpio), fidl_dispatcher_->get()),
        bindings_(std::in_place),
        outgoing_(std::in_place, fidl_dispatcher_->async_dispatcher()) {}

  zx_status_t InitAddDevice();

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

 private:
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
  void ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) override;
  void SetAltFunction(SetAltFunctionRequestView request,
                      SetAltFunctionCompleter::Sync& completer) override;
  void SetPolarity(SetPolarityRequestView request, SetPolarityCompleter::Sync& completer) override;

 private:
  const fdf::UnownedDispatcher fidl_dispatcher_;
  const uint32_t pin_;
  const uint32_t controller_id_;
  const std::string name_;

  // These objects can only be accessed on the FIDL dispatcher. Making them optional allows them to
  // be destroyed manually in our unbind hook.
  std::optional<fdf::WireClient<fuchsia_hardware_gpioimpl::GpioImpl>> gpio_;
  std::optional<fidl::ServerBindingGroup<fuchsia_hardware_gpio::Gpio>> bindings_;
  std::optional<component::OutgoingDirectory> outgoing_;
};

class GpioInitDevice;
using GpioInitDeviceType = ddk::Device<GpioInitDevice>;

class GpioInitDevice : public GpioInitDeviceType {
 public:
  static void Create(zx_device_t* parent, fdf::ClientEnd<fuchsia_hardware_gpioimpl::GpioImpl> gpio,
                     uint32_t controller_id);

  explicit GpioInitDevice(zx_device_t* parent) : GpioInitDeviceType(parent) {}

  void DdkRelease() { delete this; }

 private:
  static zx_status_t ConfigureGpios(const fuchsia_hardware_gpioimpl::wire::InitMetadata& metadata,
                                    fdf::WireSyncClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio);
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_
