// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pinimpl/cpp/driver/fidl.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/metadata/cpp/metadata_server.h>

#include <array>
#include <memory>

#define DRIVER_NAME "test-gpio"

namespace gpio {

class TestGpioDriver : public fdf::DriverBase,
                       public fdf::WireServer<fuchsia_hardware_pinimpl::PinImpl> {
 public:
  static constexpr std::string_view kChildNodeName = "test-gpio";
  static constexpr std::string_view kDriverName = "test-gpio";

  TestGpioDriver(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

 private:
  static constexpr uint32_t PIN_COUNT = 10;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_pinimpl::PinImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void Read(fuchsia_hardware_pinimpl::wire::PinImplReadRequest* request, fdf::Arena& arena,
            ReadCompleter::Sync& completer) override;
  void SetBufferMode(fuchsia_hardware_pinimpl::wire::PinImplSetBufferModeRequest* request,
                     fdf::Arena& arena, SetBufferModeCompleter::Sync& completer) override;
  void GetInterrupt(fuchsia_hardware_pinimpl::wire::PinImplGetInterruptRequest* request,
                    fdf::Arena& arena, GetInterruptCompleter::Sync& completer) override;
  void ConfigureInterrupt(fuchsia_hardware_pinimpl::wire::PinImplConfigureInterruptRequest* request,
                          fdf::Arena& arena, ConfigureInterruptCompleter::Sync& completer) override;
  void ReleaseInterrupt(fuchsia_hardware_pinimpl::wire::PinImplReleaseInterruptRequest* request,
                        fdf::Arena& arena, ReleaseInterruptCompleter::Sync& completer) override;
  void Configure(fuchsia_hardware_pinimpl::wire::PinImplConfigureRequest* request,
                 fdf::Arena& arena, ConfigureCompleter::Sync& completer) override;

  // values for our pins
  bool pins_[PIN_COUNT] = {};
  uint64_t drive_strengths_[PIN_COUNT] = {};
  fdf::ServerBindingGroup<fuchsia_hardware_pinimpl::PinImpl> bindings_;
  compat::SyncInitializedDeviceServer compat_server_;
  fidl::ClientEnd<fuchsia_driver_framework::NodeController> child_;
  fdf_metadata::MetadataServer<fuchsia_hardware_pinimpl::Metadata> pin_metadata_server_;
};

zx::result<> TestGpioDriver::Start() {
  {
    zx::result<> result = compat_server_.Initialize(
        incoming(), outgoing(), node_name(), kChildNodeName, compat::ForwardMetadata::None());
    if (result.is_error()) {
      return result.take_error();
    }
  }

  zx::result pdev = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (zx::result result = pin_metadata_server_.SetMetadataFromPDevIfExists(pdev.value());
      result.is_error()) {
    FDF_LOG(ERROR, "Failed to set SPI metadata from platform device: %s", result.status_string());
    return result.take_error();
  }
  if (zx::result result = pin_metadata_server_.Serve(*outgoing(), dispatcher());
      result.is_error()) {
    FDF_LOG(ERROR, "Failed to serve SPI metadata: %s", result.status_string());
    return result.take_error();
  }

  {
    fuchsia_hardware_pinimpl::Service::InstanceHandler handler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                          fidl::kIgnoreBindingClosure),
    });
    auto result = outgoing()->AddService<fuchsia_hardware_pinimpl::Service>(std::move(handler));
    if (result.is_error()) {
      FDF_LOG(ERROR, "AddService failed: %s", result.status_string());
      return result.take_error();
    }
  }

  std::vector offers = compat_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_pinimpl::Service>());
  offers.push_back(pin_metadata_server_.MakeOffer());
  zx::result child =
      AddChild(kChildNodeName, std::vector<fuchsia_driver_framework::NodeProperty2>{}, offers);
  if (child.is_error()) {
    FDF_LOG(ERROR, "Failed to add child: %s", child.status_string());
    return child.take_error();
  }
  child_ = std::move(child.value());

  return zx::ok();
}

void TestGpioDriver::Read(fuchsia_hardware_pinimpl::wire::PinImplReadRequest* request,
                          fdf::Arena& arena, ReadCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess(pins_[request->pin]);
  }
}

void TestGpioDriver::SetBufferMode(
    fuchsia_hardware_pinimpl::wire::PinImplSetBufferModeRequest* request, fdf::Arena& arena,
    SetBufferModeCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    if (request->mode == fuchsia_hardware_gpio::BufferMode::kOutputLow) {
      pins_[request->pin] = false;
    } else if (request->mode == fuchsia_hardware_gpio::BufferMode::kOutputHigh) {
      pins_[request->pin] = true;
    }
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDriver::GetInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplGetInterruptRequest* request, fdf::Arena& arena,
    GetInterruptCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess({});
  }
}
void TestGpioDriver::ConfigureInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplConfigureInterruptRequest* request, fdf::Arena& arena,
    ConfigureInterruptCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDriver::ReleaseInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplReleaseInterruptRequest* request, fdf::Arena& arena,
    ReleaseInterruptCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDriver::Configure(fuchsia_hardware_pinimpl::wire::PinImplConfigureRequest* request,
                               fdf::Arena& arena, ConfigureCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    if (request->config.has_drive_strength_ua()) {
      drive_strengths_[request->pin] = request->config.drive_strength_ua();
    }
    auto new_config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                          .drive_strength_ua(drive_strengths_[request->pin])
                          .Build();
    completer.buffer(arena).ReplySuccess(new_config);
  }
}

}  // namespace gpio

FUCHSIA_DRIVER_EXPORT(gpio::TestGpioDriver);
