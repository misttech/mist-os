// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpioimpl/cpp/driver/fidl.h>
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
                       public fdf::WireServer<fuchsia_hardware_gpioimpl::GpioImpl> {
 public:
  static zx_status_t Create(zx_device_t* parent);

  explicit TestGpioDevice(zx_device_t* parent) : DeviceType(parent) {}

  zx_status_t Create(std::unique_ptr<TestGpioDevice>* out);

  // Methods required by the ddk mixins
  void DdkRelease();

 private:
  static constexpr uint32_t PIN_COUNT = 10;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_gpioimpl::GpioImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void ConfigIn(fuchsia_hardware_gpioimpl::wire::GpioImplConfigInRequest* request,
                fdf::Arena& arena, ConfigInCompleter::Sync& completer) override;
  void ConfigOut(fuchsia_hardware_gpioimpl::wire::GpioImplConfigOutRequest* request,
                 fdf::Arena& arena, ConfigOutCompleter::Sync& completer) override;
  void SetAltFunction(fuchsia_hardware_gpioimpl::wire::GpioImplSetAltFunctionRequest* request,
                      fdf::Arena& arena, SetAltFunctionCompleter::Sync& completer) override;
  void Read(fuchsia_hardware_gpioimpl::wire::GpioImplReadRequest* request, fdf::Arena& arena,
            ReadCompleter::Sync& completer) override;
  void Write(fuchsia_hardware_gpioimpl::wire::GpioImplWriteRequest* request, fdf::Arena& arena,
             WriteCompleter::Sync& completer) override;
  void SetPolarity(fuchsia_hardware_gpioimpl::wire::GpioImplSetPolarityRequest* request,
                   fdf::Arena& arena, SetPolarityCompleter::Sync& completer) override;
  void SetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplSetDriveStrengthRequest* request,
                        fdf::Arena& arena, SetDriveStrengthCompleter::Sync& completer) override;
  void GetDriveStrength(fuchsia_hardware_gpioimpl::wire::GpioImplGetDriveStrengthRequest* request,
                        fdf::Arena& arena, GetDriveStrengthCompleter::Sync& completer) override;
  void GetInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplGetInterruptRequest* request,
                    fdf::Arena& arena, GetInterruptCompleter::Sync& completer) override;
  void ReleaseInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplReleaseInterruptRequest* request,
                        fdf::Arena& arena, ReleaseInterruptCompleter::Sync& completer) override;
  void GetPins(fdf::Arena& arena, GetPinsCompleter::Sync& completer) override;
  void GetInitSteps(fdf::Arena& arena, GetInitStepsCompleter::Sync& completer) override;
  void GetControllerId(fdf::Arena& arena, GetControllerIdCompleter::Sync& completer) override;

  // values for our pins
  bool pins_[PIN_COUNT] = {};
  uint64_t drive_strengths_[PIN_COUNT] = {};
  fdf::OutgoingDirectory outgoing_;
  fdf::ServerBindingGroup<fuchsia_hardware_gpioimpl::GpioImpl> bindings_;
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
    fuchsia_hardware_gpioimpl::Service::InstanceHandler handler({
        .device = dev->bindings_.CreateHandler(dev.get(), fdf::Dispatcher::GetCurrent()->get(),
                                               fidl::kIgnoreBindingClosure),
    });
    auto result = dev->outgoing_.AddService<fuchsia_hardware_gpioimpl::Service>(std::move(handler));
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

  std::array<const char*, 1> service_offers{fuchsia_hardware_gpioimpl::Service::Name};
  status = dev->DdkAdd(ddk::DeviceAddArgs("test-gpio")
                           .forward_metadata(parent, DEVICE_METADATA_GPIO_PINS)
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

void TestGpioDevice::ConfigIn(fuchsia_hardware_gpioimpl::wire::GpioImplConfigInRequest* request,
                              fdf::Arena& arena, ConfigInCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::ConfigOut(fuchsia_hardware_gpioimpl::wire::GpioImplConfigOutRequest* request,
                               fdf::Arena& arena, ConfigOutCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::SetAltFunction(
    fuchsia_hardware_gpioimpl::wire::GpioImplSetAltFunctionRequest* request, fdf::Arena& arena,
    SetAltFunctionCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::Read(fuchsia_hardware_gpioimpl::wire::GpioImplReadRequest* request,
                          fdf::Arena& arena, ReadCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess(pins_[request->index]);
  }
}

void TestGpioDevice::Write(fuchsia_hardware_gpioimpl::wire::GpioImplWriteRequest* request,
                           fdf::Arena& arena, WriteCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    pins_[request->index] = request->value;
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::GetInterrupt(
    fuchsia_hardware_gpioimpl::wire::GpioImplGetInterruptRequest* request, fdf::Arena& arena,
    GetInterruptCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess({});
  }
}

void TestGpioDevice::ReleaseInterrupt(
    fuchsia_hardware_gpioimpl::wire::GpioImplReleaseInterruptRequest* request, fdf::Arena& arena,
    ReleaseInterruptCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::SetPolarity(
    fuchsia_hardware_gpioimpl::wire::GpioImplSetPolarityRequest* request, fdf::Arena& arena,
    SetPolarityCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess();
  }
}

void TestGpioDevice::SetDriveStrength(
    fuchsia_hardware_gpioimpl::wire::GpioImplSetDriveStrengthRequest* request, fdf::Arena& arena,
    SetDriveStrengthCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    drive_strengths_[request->index] = request->ds_ua;
    completer.buffer(arena).ReplySuccess(drive_strengths_[request->index]);
  }
}

void TestGpioDevice::GetDriveStrength(
    fuchsia_hardware_gpioimpl::wire::GpioImplGetDriveStrengthRequest* request, fdf::Arena& arena,
    GetDriveStrengthCompleter::Sync& completer) {
  if (request->index >= PIN_COUNT) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  } else {
    completer.buffer(arena).ReplySuccess(drive_strengths_[request->index]);
  }
}

void TestGpioDevice::GetPins(fdf::Arena& arena, GetPinsCompleter::Sync& completer) {
  completer.buffer(arena).Reply({});
}

void TestGpioDevice::GetInitSteps(fdf::Arena& arena, GetInitStepsCompleter::Sync& completer) {
  completer.buffer(arena).Reply({});
}

void TestGpioDevice::GetControllerId(fdf::Arena& arena, GetControllerIdCompleter::Sync& completer) {
  completer.buffer(arena).Reply(0);
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
