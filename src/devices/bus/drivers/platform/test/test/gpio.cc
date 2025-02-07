// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pinimpl/cpp/driver/fidl.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>

#include <array>
#include <memory>

#include <ddktl/device.h>

#include "sdk/lib/driver/outgoing/cpp/outgoing_directory.h"

#define DRIVER_NAME "test-gpio"

namespace gpio {

class TestGpioDevice;
using DeviceType = ddk::Device<TestGpioDevice>;

class TestGpioDevice : public DeviceType,
                       public fdf::WireServer<fuchsia_hardware_pinimpl::PinImpl> {
 public:
  static zx_status_t Create(zx_device_t* parent);

  explicit TestGpioDevice(zx_device_t* parent) : DeviceType(parent) {}

  zx_status_t Create(std::unique_ptr<TestGpioDevice>* out);

  // Methods required by the ddk mixins
  void DdkRelease();

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
  fdf::OutgoingDirectory outgoing_;
  fdf::ServerBindingGroup<fuchsia_hardware_pinimpl::PinImpl> bindings_;
};

zx_status_t TestGpioDevice::Create(zx_device_t* parent) {
  auto dev = std::make_unique<TestGpioDevice>(parent);
  pdev_protocol_t pdev;
  zx_status_t status;

  zxlogf(INFO, "TestGpioDevice::Create: %s ", DRIVER_NAME);

  status = device_get_protocol(parent, ZX_PROTOCOL_PDEV, &pdev);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not get ZX_PROTOCOL_PDEV", __func__);
    return status;
  }

  {
    fuchsia_hardware_pinimpl::Service::InstanceHandler handler({
        .device = dev->bindings_.CreateHandler(dev.get(), fdf::Dispatcher::GetCurrent()->get(),
                                               fidl::kIgnoreBindingClosure),
    });
    auto result = dev->outgoing_.AddService<fuchsia_hardware_pinimpl::Service>(std::move(handler));
    if (result.is_error()) {
      zxlogf(ERROR, "AddService failed: %s", result.status_string());
      return result.error_value();
    }
  }

  auto directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (directory_endpoints.is_error()) {
    return directory_endpoints.status_value();
  }

  {
    auto result = dev->outgoing_.Serve(std::move(directory_endpoints->server));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to serve the outgoing directory: %s", result.status_string());
      return result.error_value();
    }
  }

  std::array<const char*, 1> service_offers{fuchsia_hardware_pinimpl::Service::Name};
  status = dev->DdkAdd(ddk::DeviceAddArgs("test-gpio")
                           .forward_metadata(parent, DEVICE_METADATA_GPIO_CONTROLLER)
                           .set_runtime_service_offers(service_offers)
                           .set_outgoing_dir(directory_endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAdd failed: %d", __func__, status);
    return status;
  }
  // devmgr is now in charge of dev.
  [[maybe_unused]] auto ptr = dev.release();

  return ZX_OK;
}

void TestGpioDevice::DdkRelease() { delete this; }

void TestGpioDevice::Read(fuchsia_hardware_pinimpl::wire::PinImplReadRequest* request,
                          fdf::Arena& arena, ReadCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess(pins_[request->pin]);
  }
}

void TestGpioDevice::SetBufferMode(
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

void TestGpioDevice::GetInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplGetInterruptRequest* request, fdf::Arena& arena,
    GetInterruptCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess({});
  }
}
void TestGpioDevice::ConfigureInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplConfigureInterruptRequest* request, fdf::Arena& arena,
    ConfigureInterruptCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::ReleaseInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplReleaseInterruptRequest* request, fdf::Arena& arena,
    ReleaseInterruptCompleter::Sync& completer) {
  if (request->pin >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::Configure(fuchsia_hardware_pinimpl::wire::PinImplConfigureRequest* request,
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

zx_status_t test_gpio_bind(void* ctx, zx_device_t* parent) {
  return TestGpioDevice::Create(parent);
}

constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = test_gpio_bind;
  return driver_ops;
}();

}  // namespace gpio

ZIRCON_DRIVER(test_gpio, gpio::driver_ops, "zircon", "0.1");
