// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/sherlock/sherlock.h"

#include <assert.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <array>

#include <bind/fuchsia/amlogic/platform/t931/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/platform/bus/cpp/bind.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

#include "src/devices/board/drivers/sherlock/sherlock-bind.h"
#include "src/devices/board/drivers/sherlock/sherlock-gpios.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Sherlock::Create(void* ctx, zx_device_t* parent) {
  iommu_protocol_t iommu;

  auto endpoints = fdf::CreateEndpoints<fpbus::PlatformBus>();
  if (endpoints.is_error()) {
    return endpoints.error_value();
  }

  zx_status_t status = device_connect_runtime_protocol(
      parent, fpbus::Service::PlatformBus::ServiceName, fpbus::Service::PlatformBus::Name,
      endpoints->server.TakeHandle().release());
  if (status != ZX_OK) {
    return status;
  }

  fdf::WireSyncClient<fpbus::PlatformBus> pbus(std::move(endpoints->client));

  status = device_get_protocol(parent, ZX_PROTOCOL_IOMMU, &iommu);
  if (status != ZX_OK) {
    return status;
  }

  fbl::AllocChecker ac;
  auto board = fbl::make_unique_checked<Sherlock>(&ac, parent, pbus.TakeClientEnd(), &iommu);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  {
    fuchsia_hardware_platform_bus::Service::InstanceHandler handler({
        .platform_bus = fit::bind_member<&Sherlock::Serve>(board.get()),
    });
    auto result =
        board->outgoing_.AddService<fuchsia_hardware_platform_bus::Service>(std::move(handler));
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
    auto result = board->outgoing_.Serve(std::move(directory_endpoints->server));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to serve the outgoing directory: %s", result.status_string());
      return result.error_value();
    }
  }

  constexpr zx_device_prop_t kBoardDriverProps[] = {
      {BIND_PLATFORM_DEV_VID, 0, bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE},
      {BIND_PLATFORM_DEV_DID, 0, bind_fuchsia_google_platform::BIND_PLATFORM_DEV_DID_POST_INIT},
  };

  std::array<const char*, 1> fidl_service_offers{fuchsia_hardware_platform_bus::Service::Name};
  status = board->DdkAdd(ddk::DeviceAddArgs("sherlock")
                             .set_props(kBoardDriverProps)
                             .set_outgoing_dir(directory_endpoints->client.TakeChannel())
                             .set_runtime_service_offers(fidl_service_offers));
  if (status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dummy = board.release();
  return ZX_OK;
}

int Sherlock::Start() {
  // Load protocol implementation drivers first.
  if (SysmemInit() != ZX_OK) {
    zxlogf(ERROR, "SysmemInit() failed");
    return -1;
  }

  if (I2cInit() != ZX_OK) {
    zxlogf(ERROR, "I2cInit() failed");
  }

  if (SpiInit() != ZX_OK) {
    zxlogf(ERROR, "SpiInit() failed");
  }

  if (ThermalInit() != ZX_OK) {
    zxlogf(ERROR, "ThermalInit() failed");
  }

  if (EmmcInit() != ZX_OK) {
    zxlogf(ERROR, "EmmcInit() failed");
  }

  if (SdioInit() != ZX_OK) {
    zxlogf(ERROR, "SdioInit() failed");
  }

  if (BluetoothInit() != ZX_OK) {
    zxlogf(ERROR, "BluetoothInit() failed");
  }

  if (CameraInit() != ZX_OK) {
    zxlogf(ERROR, "CameraInit() failed");
  }

  if (AudioInit() != ZX_OK) {
    zxlogf(ERROR, "AudioInit() failed");
  }

  if (LightInit() != ZX_OK) {
    zxlogf(ERROR, "LightInit() failed");
    return -1;
  }

  // ClkInit() must be called after other subsystems that bind to clock have had a chance to add
  // their init steps.
  if (ClkInit() != ZX_OK) {
    zxlogf(ERROR, "ClkInit() failed");
    return -1;
  }

  // GpioInit() must be called after other subsystems that bind to GPIO have had a chance to add
  // their init steps.
  if (GpioInit() != ZX_OK) {
    zxlogf(ERROR, "GpioInit() failed");
    return -1;
  }

  if (AddPostInitDevice() != ZX_OK) {
    zxlogf(ERROR, "AddPostInitDevice() failed");
    return -1;
  }

  if (RegistersInit() != ZX_OK) {
    zxlogf(ERROR, "RegistersInit() failed");
    return -1;
  }

  if (CpuInit() != ZX_OK) {
    zxlogf(ERROR, "CpuInit() failed\n");
  }

  if (CanvasInit() != ZX_OK) {
    zxlogf(ERROR, "CanvasInit() failed");
  }

  if (PwmInit() != ZX_OK) {
    zxlogf(ERROR, "PwmInit() failed");
  }

  if (DsiInit() != ZX_OK) {
    zxlogf(ERROR, "DsiInit() failed");
  }

  // Then the platform device drivers.
  if (UsbInit() != ZX_OK) {
    zxlogf(ERROR, "UsbInit() failed");
  }

  if (TeeInit() != ZX_OK) {
    zxlogf(ERROR, "TeeInit() failed");
  }

  if (VideoInit() != ZX_OK) {
    zxlogf(ERROR, "VideoInit() failed");
  }

  if (VideoEncInit() != ZX_OK) {
    zxlogf(ERROR, "VideoEncInit() failed");
  }

  if (HevcEncInit() != ZX_OK) {
    zxlogf(ERROR, "HevcEncInit() failed");
  }

  if (MaliInit() != ZX_OK) {
    zxlogf(ERROR, "MaliInit() failed");
  }

  if (NnaInit() != ZX_OK) {
    zxlogf(ERROR, "NnaInit() failed");
  }

  if (ButtonsInit() != ZX_OK) {
    zxlogf(ERROR, "ButtonsInit() failed");
  }

  if (OtRadioInit() != ZX_OK) {
    zxlogf(ERROR, "OtRadioInit() failed");
  }

  if (SecureMemInit() != ZX_OK) {
    zxlogf(ERROR, "SecureMbemInit failed");
  }

  if (BacklightInit() != ZX_OK) {
    zxlogf(ERROR, "BacklightInit() failed");
  }

  if (RamCtlInit() != ZX_OK) {
    zxlogf(ERROR, "RamCtlInit failed");
  }

  if (auto result = AdcInit(); result.is_error()) {
    zxlogf(ERROR, "AdcInit failed: %d", result.error_value());
  }

  if (ThermistorInit() != ZX_OK) {
    zxlogf(ERROR, "ThermistorInit failed");
  }

  ZX_ASSERT_MSG(clock_init_steps_.empty(), "Clock init steps added but not applied");
  ZX_ASSERT_MSG(gpio_init_steps_.empty(), "GPIO init steps added but not applied");

  return 0;
}

void Sherlock::DdkInit(ddk::InitTxn txn) { txn.Reply(Start() == 0 ? ZX_OK : ZX_ERR_INTERNAL); }

void Sherlock::DdkRelease() { delete this; }

zx_status_t Sherlock::AddPostInitDevice() {
  constexpr std::array<uint32_t, 7> kPostInitGpios{
      bind_fuchsia_amlogic_platform_t931::GPIOA_PIN_ID_PIN_11,
      bind_fuchsia_amlogic_platform_t931::GPIOA_PIN_ID_PIN_12,
      bind_fuchsia_amlogic_platform_t931::GPIOC_PIN_ID_PIN_4,
      bind_fuchsia_amlogic_platform_t931::GPIOC_PIN_ID_PIN_5,
      bind_fuchsia_amlogic_platform_t931::GPIOC_PIN_ID_PIN_6,
      bind_fuchsia_amlogic_platform_t931::GPIOH_PIN_ID_PIN_0,
      bind_fuchsia_amlogic_platform_t931::GPIOH_PIN_ID_PIN_2,
  };

  const ddk::BindRule post_init_rules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia_hardware_platform_bus::SERVICE,
                              bind_fuchsia_hardware_platform_bus::SERVICE_DRIVERTRANSPORT),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_google_platform::BIND_PLATFORM_DEV_DID_POST_INIT),
  };
  const device_bind_prop_t post_init_properties[] = {
      ddk::MakeProperty(bind_fuchsia_hardware_platform_bus::SERVICE,
                        bind_fuchsia_hardware_platform_bus::SERVICE_DRIVERTRANSPORT),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                        bind_fuchsia_google_platform::BIND_PLATFORM_DEV_DID_POST_INIT),
  };

  auto spec = ddk::CompositeNodeSpec(post_init_rules, post_init_properties);
  for (const uint32_t pin : kPostInitGpios) {
    const ddk::BindRule gpio_rules[] = {
        ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
        ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, pin),
    };
    const device_bind_prop_t gpio_properties[] = {
        ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_gpio::BIND_PROTOCOL_DEVICE),
        ddk::MakeProperty(bind_fuchsia::GPIO_PIN, pin),
    };
    spec.AddParentSpec(gpio_rules, gpio_properties);
  }

  if (zx_status_t status = DdkAddCompositeNodeSpec("post-init", spec); status != ZX_OK) {
    zxlogf(ERROR, "Failed to add board info composite: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = Sherlock::Create;
  return ops;
}();

}  // namespace sherlock

ZIRCON_DRIVER(sherlock, sherlock::driver_ops, "zircon", "0.1");
