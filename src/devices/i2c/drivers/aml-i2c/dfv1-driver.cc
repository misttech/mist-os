// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dfv1-driver.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <zircon/threads.h>

#include <soc/aml-common/aml-i2c.h>

#include "zircon/status.h"

namespace aml_i2c {

zx::result<aml_i2c_delay_values> GetDelay(zx_device_t* parent) {
  aml_i2c_delay_values delay{0, 0};
  size_t actual;
  zx_status_t status = device_get_fragment_metadata(parent, "pdev", DEVICE_METADATA_PRIVATE, &delay,
                                                    sizeof(delay), &actual);
  if (status != ZX_OK) {
    if (status != ZX_ERR_NOT_FOUND) {
      zxlogf(ERROR, "device_get_fragment_metadata failed: %s", zx_status_get_string(status));
      return zx::error(status);
    }
    zxlogf(DEBUG, "Metadata not found; using default delay values");
  } else if (actual != sizeof(delay)) {
    zxlogf(ERROR, "metadata size mismatch");
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(delay);
}

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> Dfv1Driver::ServeI2cImpl() {
  zx::result result = outgoing_.AddService<fuchsia_hardware_i2cimpl::Service>(
      aml_i2c_->GetI2cImplInstanceHandler(fdf::Dispatcher::GetCurrent()->get()));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory: %s", result.status_string());
    return result.take_error();
  }

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.take_error();
  }

  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve the outgoing directory: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok(std::move(endpoints.value().client));
}

zx_status_t Dfv1Driver::Bind(void* ctx, zx_device_t* parent) {
  ddk::PDevFidl pdev(parent, "pdev");
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "ZX_PROTOCOL_PDEV not available");
    return ZX_ERR_NO_RESOURCES;
  }

  zx::result delay = GetDelay(parent);
  if (delay.is_error()) {
    zxlogf(ERROR, "Failed to get delay: %s", delay.status_string());
    return delay.status_value();
  }

  zx::result aml_i2c = AmlI2c::Create(pdev, delay.value());
  if (aml_i2c.is_error() != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize: %s", aml_i2c.status_string());
    return aml_i2c.status_value();
  }

  auto driver = std::make_unique<Dfv1Driver>(parent, std::move(aml_i2c.value()));
  zx::result outgoing_client = driver->ServeI2cImpl();
  if (outgoing_client.is_error()) {
    zxlogf(ERROR, "Failed to server i2c impl fidl protocol: %s", outgoing_client.status_string());
    return outgoing_client.status_value();
  }

  std::array offers = {
      fuchsia_hardware_i2cimpl::Service::Name,
  };
  zx_status_t status = driver->DdkAdd(ddk::DeviceAddArgs("aml-i2c")
                                          .set_runtime_service_offers(offers)
                                          .set_outgoing_dir(outgoing_client->TakeChannel())
                                          .forward_metadata(parent, DEVICE_METADATA_I2C_CHANNELS));
  if (status != ZX_OK) {
    zxlogf(ERROR, "device_add failed");
    return status;
  }

  [[maybe_unused]] auto* unused = driver.release();
  return status;
}

Dfv1Driver::Dfv1Driver(zx_device_t* parent, std::unique_ptr<AmlI2c> aml_i2c)
    : DeviceType(parent), aml_i2c_(std::move(aml_i2c)) {
  // Set role for IRQ thread.
  const char* kRoleName = "fuchsia.devices.i2c.drivers.aml-i2c.interrupt";
  zx_status_t status = device_set_profile_by_role(zxdev(), thrd_get_zx_handle(aml_i2c_->irqthrd()),
                                                  kRoleName, strlen(kRoleName));
  if (status != ZX_OK) {
    zxlogf(WARNING, "Failed to apply role: %s", zx_status_get_string(status));
  }
}

}  // namespace aml_i2c

static zx_driver_ops_t aml_i2c_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = aml_i2c::Dfv1Driver::Bind,
};

ZIRCON_DRIVER(aml_i2c, aml_i2c_driver_ops, "zircon", "0.1");
