// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c.h"

#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>

#include <memory>

#include <fbl/alloc_checker.h>

#include "i2c-bus.h"
#include "src/devices/i2c/drivers/i2c/i2c_bind.h"

namespace i2c {

zx_status_t I2cDevice::Create(void* ctx, zx_device_t* parent) {
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

  status = device->Init(i2c, bus_base, bus_count);

  [[maybe_unused]] auto* unused = device.release();

  return status;
}

zx_status_t I2cDevice::Init(const ddk::I2cImplProtocolClient& i2c, const uint32_t bus_base,
                            const uint32_t bus_count) {
  ZX_DEBUG_ASSERT(metadata_->has_channels());

  zxlogf(DEBUG, "%zu channels supplied.", metadata_->channels().count());

  dispatchers_.reserve(bus_count);

  for (uint32_t i = 0; i < bus_count; i++) {
    dispatchers_.emplace_back(new async::Loop(&kAsyncLoopConfigNeverAttachToThread));

    char name[32];
    snprintf(name, sizeof(name), "I2cBus[%u]", i + bus_base);

    if (zx_status_t status = dispatchers_.back()->StartThread(name); status != ZX_OK) {
      zxlogf(ERROR, "Failed to start dispatcher: %s", zx_status_get_string(status));
      return status;
    }

    async_dispatcher_t* const dispatcher = dispatchers_.back()->dispatcher();

    if (zx_status_t status = SetSchedulerRole(i + bus_base, dispatcher); status != ZX_OK) {
      return status;
    }

    zx_status_t status =
        I2cBus::CreateAndAddChildren(zxdev(), i2c, i + bus_base, metadata_->channels(), dispatcher);
    if (status != ZX_OK) {
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t I2cDevice::SetSchedulerRole(const uint32_t bus_id,
                                        async_dispatcher_t* const dispatcher) {
  zx_status_t status = async::PostTask(dispatcher, [device = zxdev(), bus_id]() {
    char name[32];
    snprintf(name, sizeof(name), "I2cBus[%u]", bus_id);

    // Set role for bus transaction thread.
    const char* role_name = "fuchsia.devices.i2c.drivers.i2c.bus";
    zx_status_t status =
        device_set_profile_by_role(device, zx::thread::self()->get(), role_name, strlen(role_name));
    if (status != ZX_OK) {
      zxlogf(WARNING, "Failed to apply role to dispatch thread: %s", zx_status_get_string(status));
    }
  });

  if (status != ZX_OK) {
    zxlogf(WARNING, "Failed to post deadline profile task: %s", zx_status_get_string(status));
  }

  return status;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = I2cDevice::Create;
  return ops;
}();

}  // namespace i2c

ZIRCON_DRIVER(i2c, i2c::driver_ops, "zircon", "0.1");
