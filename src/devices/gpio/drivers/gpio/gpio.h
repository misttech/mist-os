// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_
#define SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.pin/cpp/fidl.h>
#include <fidl/fuchsia.hardware.pinimpl/cpp/driver/fidl.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <stdio.h>

#include <optional>
#include <string>
#include <string_view>

#include <fbl/intrusive_double_list.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

namespace gpio {

class GpioDevice : public fidl::WireServer<fuchsia_hardware_pin::Pin>,
                   public fidl::WireServer<fuchsia_hardware_pin::Debug> {
 public:
  GpioDevice(fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl> pinimpl, uint32_t pin,
             uint32_t controller_id, std::string_view name)
      : fidl_dispatcher_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        pin_(pin),
        controller_id_(controller_id),
        name_(name),
        pinimpl_(std::move(pinimpl)),
        devfs_connector_(fit::bind_member<&GpioDevice::DevfsConnect>(this)) {}

  zx::result<> AddServices(const std::shared_ptr<fdf::Namespace>& incoming,
                           const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                           const std::optional<std::string>& node_name);

  zx::result<> AddDevice(fidl::UnownedClientEnd<fuchsia_driver_framework::Node> root_node,
                         fdf::Logger& logger);

 private:
  class GpioInstance : public fbl::RefCounted<GpioInstance>,
                       public fbl::DoublyLinkedListable<fbl::RefPtr<GpioInstance>,
                                                        fbl::NodeOptions::AllowRemoveFromContainer>,
                       public fidl::WireServer<fuchsia_hardware_gpio::Gpio> {
   public:
    GpioInstance(async_dispatcher_t* dispatcher,
                 fidl::ServerEnd<fuchsia_hardware_gpio::Gpio> server_end,
                 fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl> pinimpl, uint32_t pin,
                 const GpioDevice* parent)
        : binding_(dispatcher, std::move(server_end), this,
                   fit::bind_member<&GpioInstance::OnUnbound>(this)),
          pinimpl_(std::move(pinimpl)),
          pin_(pin),
          parent_(parent) {}

    // Returns true if this GPIO instance has an interrupt or a pending call to get or release one.
    bool has_interrupt() const { return interrupt_state_ != InterruptState::kNoInterrupt; }

   private:
    // These states are used to track the progress of async pinimpl interrupt calls, and to prevent
    // simultaneous calls to the corresponding GPIO methods. They also determine the action to take
    // when the GPIO client unbinds.
    enum class InterruptState {
      kNoInterrupt,         // This instance does not have an interrupt or any pending calls.
      kGettingInterrupt,    // This instance has a pending call to GetInterrupt().
      kHasInterrupt,        // This instance has an interrupt and no pending calls.
      kReleasingInterrupt,  // This instance has a pending call to ReleaseInterrupt().
    };

    void Read(ReadCompleter::Sync& completer) override;
    void SetBufferMode(SetBufferModeRequestView request,
                       SetBufferModeCompleter::Sync& completer) override;
    void GetInterrupt(GetInterruptRequestView request,
                      GetInterruptCompleter::Sync& completer) override;
    void ConfigureInterrupt(fuchsia_hardware_gpio::wire::GpioConfigureInterruptRequest* request,
                            ConfigureInterruptCompleter::Sync& completer) override;
    void ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) override;

    void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_gpio::Gpio> metadata,
                               fidl::UnknownMethodCompleter::Sync& completer) override;

    void OnUnbound(fidl::UnbindInfo info);

    // Call into the parent to release the instance. ReleaseInterrupt() is called first if needed.
    void ReleaseInstance();

    fidl::ServerBinding<fuchsia_hardware_gpio::Gpio> binding_;
    fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl> pinimpl_;
    const uint32_t pin_;
    const GpioDevice* const parent_;
    InterruptState interrupt_state_ = InterruptState::kNoInterrupt;
    bool release_instance_after_call_completes_ = false;
  };

  // Returns true if any GPIO instance has an interrupt or a pending call to get or release one.
  bool gpio_instance_has_interrupt() const;

  void DevfsConnect(fidl::ServerEnd<fuchsia_hardware_pin::Debug> server);

  void Configure(fuchsia_hardware_pin::wire::PinConfigureRequest* request,
                 ConfigureCompleter::Sync& completer) override;

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_pin::Pin> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void ConnectPin(fuchsia_hardware_pin::wire::DebugConnectPinRequest* request,
                  ConnectPinCompleter::Sync& completer) override;
  void ConnectGpio(fuchsia_hardware_pin::wire::DebugConnectGpioRequest* request,
                   ConnectGpioCompleter::Sync& completer) override;

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_pin::Debug> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

  void ConnectGpio(fidl::ServerEnd<fuchsia_hardware_gpio::Gpio> server);

  std::string pin_name() const {
    char name[20];
    snprintf(name, sizeof(name), "gpio-%u", pin_);
    return name;
  }

  async_dispatcher_t* const fidl_dispatcher_;
  const uint32_t pin_;
  const uint32_t controller_id_;
  const std::string name_;

  fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl> pinimpl_;
  fbl::DoublyLinkedList<fbl::RefPtr<GpioInstance>> gpio_instances_;
  fidl::ServerBindingGroup<fuchsia_hardware_pin::Pin> pin_bindings_;
  fidl::ServerBindingGroup<fuchsia_hardware_pin::Debug> debug_bindings_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fuchsia_hardware_pin::Debug> devfs_connector_;
};

class GpioInitDevice {
 public:
  static std::unique_ptr<GpioInitDevice> Create(
      std::span<fuchsia_hardware_pinimpl::InitStep> init_steps,
      fidl::UnownedClientEnd<fuchsia_driver_framework::Node> node, fdf::Logger& logger,
      uint32_t controller_id, fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl>& pinimpl);

 private:
  static zx_status_t ConfigureGpios(
      std::span<fuchsia_hardware_pinimpl::InitStep> init_steps,
      fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl>& pinimpl);

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
  void CreatePinDevices(uint32_t controller_id, std::span<fuchsia_hardware_pinimpl::Pin> pins,
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
  fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl> pinimpl_;
  std::vector<std::unique_ptr<GpioDevice>> children_;
  std::unique_ptr<GpioInitDevice> init_device_;

  fdf::OwnedChildNode node_;
};

}  // namespace gpio

#endif  // SRC_DEVICES_GPIO_DRIVERS_GPIO_GPIO_H_
