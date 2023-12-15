// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/drivers/nelson/nelson.h"

#include <assert.h>
#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <array>

#include <bind/fuchsia/amlogic/platform/s905d3/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/platform/bus/cpp/bind.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

#include "src/devices/board/drivers/nelson/nelson-bind.h"
#include "src/devices/board/drivers/nelson/nelson-gpios.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;
namespace fhgpio = fuchsia_hardware_gpio;

int Nelson::Thread() {
  zx_status_t status;

  // Sysmem is started early so zx_vmo_create_contiguous() works.
  if ((status = SysmemInit()) != ZX_OK) {
    zxlogf(ERROR, "%s: SysmemInit() failed: %d", __func__, status);
    return status;
  }

  if ((status = I2cInit()) != ZX_OK) {
    zxlogf(ERROR, "I2cInit failed: %d", status);
  }

  if ((status = SpiInit()) != ZX_OK) {
    zxlogf(ERROR, "SpiInit failed: %d", status);
  }

  if ((status = EmmcInit()) != ZX_OK) {
    zxlogf(ERROR, "EmmcInit() failed: %d", status);
  }

  if ((status = SdioInit()) != ZX_OK) {
    zxlogf(ERROR, "SdioInit failed: %d", status);
  }

  if ((status = LightInit()) != ZX_OK) {
    zxlogf(ERROR, "LightInit failed: %d", status);
  }

  if ((status = ThermalInit()) != ZX_OK) {
    zxlogf(ERROR, "ThermalInit failed: %d", status);
  }

  if ((status = AudioInit()) != ZX_OK) {
    zxlogf(ERROR, "AudioInit failed: %d", status);
  }

  if (OtRadioInit() != ZX_OK) {
    zxlogf(ERROR, "OtRadioInit failed");
  }

  if ((status = BluetoothInit()) != ZX_OK) {
    zxlogf(ERROR, "BluetoothInit failed: %d", status);
  }

  // ClkInit() must be called after other subsystems that bind to clock have had a chance to add
  // their init steps.
  if ((status = ClkInit()) != ZX_OK) {
    zxlogf(ERROR, "ClkInit failed: %d", status);
  }
  clock_init_steps_.clear();

  // GpioInit() must be called after other subsystems that bind to GPIO have had a chance to add
  // their init steps.
  if ((status = GpioInit()) != ZX_OK) {
    zxlogf(ERROR, "%s: GpioInit() failed: %d", __func__, status);
    return status;
  }
  gpio_init_steps_.clear();

  if ((status = AddPostInitDevice()) != ZX_OK) {
    zxlogf(ERROR, "%s: AddPostInitDevice() failed: %d", __func__, status);
    return status;
  }

  if ((status = RegistersInit()) != ZX_OK) {
    zxlogf(ERROR, "RegistersInit failed: %d", status);
  }

  if ((status = ButtonsInit()) != ZX_OK) {
    zxlogf(ERROR, "ButtonsInit failed: %d", status);
  }

  if ((status = CpuInit()) != ZX_OK) {
    zxlogf(ERROR, "CpuInit failed: %d", status);
  }

  if ((status = MaliInit()) != ZX_OK) {
    zxlogf(ERROR, "MaliInit failed: %d", status);
  }

  if ((status = UsbInit()) != ZX_OK) {
    zxlogf(ERROR, "UsbInit failed: %d", status);
  }

  if ((status = TouchInit()) != ZX_OK) {
    zxlogf(ERROR, "TouchInit failed: %d", status);
  }

  if ((status = DsiInit()) != ZX_OK) {
    zxlogf(ERROR, "DsiInit failed: %d", status);
  }

  if ((status = CanvasInit()) != ZX_OK) {
    zxlogf(ERROR, "CanvasInit failed: %d", status);
  }

  if ((status = PwmInit()) != ZX_OK) {
    zxlogf(ERROR, "PwmInit failed: %d", status);
  }

  if ((status = TeeInit()) != ZX_OK) {
    zxlogf(ERROR, "TeeInit failed: %d", status);
  }

  if ((status = VideoInit()) != ZX_OK) {
    zxlogf(ERROR, "VideoInit failed: %d", status);
  }

  if ((status = SecureMemInit()) != ZX_OK) {
    zxlogf(ERROR, "SecureMemInit failed: %d", status);
  }

  if ((status = BacklightInit()) != ZX_OK) {
    zxlogf(ERROR, "BacklightInit failed: %d", status);
  }

  if ((status = PowerInit()) != ZX_OK) {
    zxlogf(ERROR, "PowerInit failed: %d", status);
  }

  if ((status = NnaInit()) != ZX_OK) {
    zxlogf(ERROR, "NnaInit failed: %d", status);
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

  return ZX_OK;
}

zx_status_t Nelson::Start() {
  int rc = thrd_create_with_name(
      &thread_, [](void* arg) -> int { return reinterpret_cast<Nelson*>(arg)->Thread(); }, this,
      "nelson-start-thread");
  if (rc != thrd_success) {
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

void Nelson::DdkRelease() { delete this; }

zx_status_t Nelson::Create(void* ctx, zx_device_t* parent) {
  iommu_protocol_t iommu;
  auto endpoints = fdf::CreateEndpoints<fuchsia_hardware_platform_bus::PlatformBus>();
  if (endpoints.is_error()) {
    return endpoints.error_value();
  }

  zx_status_t status = device_connect_runtime_protocol(
      parent, fpbus::Service::PlatformBus::ServiceName, fpbus::Service::PlatformBus::Name,
      endpoints->server.TakeHandle().release());
  if (status != ZX_OK) {
    return status;
  }

  status = device_get_protocol(parent, ZX_PROTOCOL_IOMMU, &iommu);
  if (status != ZX_OK) {
    return status;
  }

  fbl::AllocChecker ac;
  auto board = fbl::make_unique_checked<Nelson>(&ac, parent, std::move(endpoints->client), &iommu);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  {
    fuchsia_hardware_platform_bus::Service::InstanceHandler handler({
        .platform_bus = fit::bind_member<&Nelson::Serve>(board.get()),
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
      {BIND_PLATFORM_DEV_PID, 0, bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_NELSON},
      {BIND_PLATFORM_DEV_INSTANCE_ID, 0, 1},
  };

  const char* fidl_service_offers[] = {fuchsia_hardware_platform_bus::Service::Name};
  status = board->DdkAdd(ddk::DeviceAddArgs("nelson")
                             .set_props(kBoardDriverProps)
                             .set_outgoing_dir(directory_endpoints->client.TakeChannel())
                             .set_runtime_service_offers({fidl_service_offers, 1})
                             .forward_metadata(parent, DEVICE_METADATA_BOARD_PRIVATE));
  if (status != ZX_OK) {
    return status;
  }

  // Start up our protocol helpers and platform devices.
  status = board->Start();
  if (status == ZX_OK) {
    // devmgr is now in charge of the device.
    [[maybe_unused]] auto* dummy = board.release();
  }
  return status;
}

zx_status_t Nelson::AddPostInitDevice() {
  constexpr std::array<uint32_t, 8> kPostInitGpios{
      bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_7,
      bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_8,
      bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_3,
      bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_0,
      bind_fuchsia_amlogic_platform_s905d3::GPIOAO_PIN_ID_PIN_4,
      bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_11,
      bind_fuchsia_amlogic_platform_s905d3::GPIOZ_PIN_ID_PIN_12,
      bind_fuchsia_amlogic_platform_s905d3::GPIOH_PIN_ID_PIN_8,
  };

  const ddk::BindRule post_init_rules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia_hardware_platform_bus::SERVICE,
                              bind_fuchsia_hardware_platform_bus::SERVICE_DRIVERTRANSPORT),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                              bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_NELSON),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, 1u),
  };
  const device_bind_prop_t post_init_properties[] = {
      ddk::MakeProperty(bind_fuchsia_hardware_platform_bus::SERVICE,
                        bind_fuchsia_hardware_platform_bus::SERVICE_DRIVERTRANSPORT),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                        bind_fuchsia_google_platform::BIND_PLATFORM_DEV_PID_NELSON),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, 1u),
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

static zx_driver_ops_t nelson_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = Nelson::Create;
  return ops;
}();

}  // namespace nelson

ZIRCON_DRIVER(nelson, nelson::nelson_driver_ops, "zircon", "0.1");
