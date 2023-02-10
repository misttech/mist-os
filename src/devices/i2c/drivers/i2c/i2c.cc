// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <memory>

#include <fbl/alloc_checker.h>

#include "i2c-bus.h"
#include "src/devices/i2c/drivers/i2c/i2c_bind.h"

namespace i2c {

zx_status_t I2cDevice::Create(void* ctx, zx_device_t* parent) {
  return Create(ctx, parent, fdf::Dispatcher::GetCurrent()->async_dispatcher());
}

zx_status_t I2cDevice::Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher) {
  ddk::I2cImplProtocolClient i2c(parent);
  if (!i2c.is_valid()) {
    zxlogf(ERROR, "Failed to get i2cimpl client");
    return ZX_ERR_NO_RESOURCES;
  }

  const uint32_t bus_base = i2c.GetBusBase();

  const uint32_t bus_count = i2c.GetBusCount();
  if (!bus_count) {
    zxlogf(ERROR, "i2cimpl driver has no buses");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata>(
      parent, DEVICE_METADATA_I2C_CHANNELS);
  if (!decoded.is_ok()) {
    zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    return decoded.error_value();
  }

  if (!decoded.value()->has_channels()) {
    zxlogf(ERROR, "No channels supplied");
    return ZX_ERR_NO_RESOURCES;
  }

  const fidl::VectorView<fuchsia_hardware_i2c_businfo::wire::I2CChannel>& channels =
      decoded.value()->channels();

  for (auto& channel : channels) {
    const uint32_t bus_id = channel.has_bus_id() ? channel.bus_id() : 0;
    if (bus_id < bus_base || (bus_id - bus_base) >= bus_count) {
      zxlogf(ERROR, "Bus ID %u out of range", bus_id);
      return ZX_ERR_INVALID_ARGS;
    }
  }

  fbl::AllocChecker ac;
  std::unique_ptr<I2cDevice> device(new (&ac) I2cDevice(parent, std::move(decoded.value())));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = device->DdkAdd(ddk::DeviceAddArgs("i2c").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    return status;
  }

  status = device->Init(i2c, bus_base, bus_count, dispatcher);

  [[maybe_unused]] auto* unused = device.release();

  return status;
}

zx_status_t I2cDevice::Init(const ddk::I2cImplProtocolClient& i2c, const uint32_t bus_base,
                            const uint32_t bus_count, async_dispatcher_t* const dispatcher) {
  zxlogf(DEBUG, "%zu channels supplied.", metadata_->channels().count());

  for (uint32_t i = 0; i < bus_count; i++) {
    zx_status_t status =
        I2cBus::CreateAndAddChildren(zxdev(), i2c, i + bus_base, metadata_->channels(), dispatcher);
    if (status != ZX_OK) {
      return status;
    }
  }

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = I2cDevice::Create;
  return ops;
}();

}  // namespace i2c

ZIRCON_DRIVER(i2c, i2c::driver_ops, "zircon", "0.1");
