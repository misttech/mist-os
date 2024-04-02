// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>

#include <cstring>
#include <memory>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/khadas/platform/cpp/bind.h>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {

const ddk::BindRule kI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                            bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, bind_fuchsia_i2c::BIND_I2C_BUS_ID_I2C_A0_0),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, bind_fuchsia_i2c::BIND_I2C_ADDRESS_MCU),
};

const device_bind_prop_t kI2cProperties[] = {
    ddk::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
};

const ddk::BindRule kMcuRules[] = {
    ddk::MakeAcceptBindRule(BIND_PLATFORM_DEV_VID,
                            bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_VID_KHADAS),
    ddk::MakeAcceptBindRule(BIND_PLATFORM_DEV_PID,
                            bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_PID_VIM3),
    ddk::MakeAcceptBindRule(BIND_PLATFORM_DEV_DID,
                            bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_DID_VIM3_MCU),

};

const device_bind_prop_t kMcuProperties[] = {
    ddk::MakeProperty(BIND_PLATFORM_DEV_VID,
                      bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_VID_KHADAS),
    ddk::MakeProperty(BIND_PLATFORM_DEV_PID,
                      bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_PID_VIM3),
    ddk::MakeProperty(BIND_PLATFORM_DEV_DID,
                      bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_DID_VIM3_MCU),
};

const zx_device_prop_t kMcuProps[] = {
    {BIND_PLATFORM_DEV_VID, 0, bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_VID_KHADAS},
    {BIND_PLATFORM_DEV_PID, 0, bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_PID_VIM3},
    {BIND_PLATFORM_DEV_DID, 0, bind_fuchsia_khadas_platform::BIND_PLATFORM_DEV_DID_VIM3_MCU},
};

zx_status_t Vim3::McuInit() {
  auto dev = std::make_unique<Vim3Child>(zxdev());
  zx_status_t status = dev->DdkAdd(ddk::DeviceAddArgs("mcu").set_props(kMcuProps));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd mcu request failed: %s", zx_status_get_string(status));
    return status;
  }

  // Device intentionally leaked as it is now held by DevMgr.
  [[maybe_unused]] auto ptr = dev.release();

  status = DdkAddCompositeNodeSpec(
      "mcu-composite",
      ddk::CompositeNodeSpec(kI2cRules, kI2cProperties).AddParentSpec(kMcuRules, kMcuProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

}  // namespace vim3
