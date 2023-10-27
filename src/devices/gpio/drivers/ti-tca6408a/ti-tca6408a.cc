// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ti-tca6408a.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>

namespace {

// Arbitrary values for I2C retries.
constexpr uint8_t kI2cRetries = 10;
constexpr zx::duration kI2cRetryDelay = zx::usec(1);

}  // namespace

namespace gpio {

zx_status_t TiTca6408a::Create(void* ctx, zx_device_t* parent) {
  ddk::I2cChannel i2c(parent, "i2c");
  if (!i2c.is_valid()) {
    zxlogf(ERROR, "Failed to get I2C channel");
    return ZX_ERR_NO_RESOURCES;
  }

  {
    // Clear the polarity inversion register.
    const uint8_t write_buf[2] = {static_cast<uint8_t>(Register::kPolarityInversion), 0};
    i2c.WriteSyncRetries(write_buf, sizeof(write_buf), kI2cRetries, kI2cRetryDelay);
  }

  uint32_t pin_index_offset = 0;
  size_t actual = 0;
  zx_status_t status = device_get_metadata(parent, DEVICE_METADATA_PRIVATE, &pin_index_offset,
                                           sizeof(pin_index_offset), &actual);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get metadata: %s", zx_status_get_string(status));
    return status;
  }

  if (actual != sizeof(pin_index_offset)) {
    zxlogf(ERROR, "Unexpected metadata size: got %zu, expected %zu", actual,
           sizeof(pin_index_offset));
    return ZX_ERR_INTERNAL;
  }

  auto dev = std::make_unique<TiTca6408a>(parent, std::move(i2c), pin_index_offset);
  if ((status = dev->DdkAdd(ddk::DeviceAddArgs("ti-tca6408a")
                                .forward_metadata(parent, DEVICE_METADATA_GPIO_PINS))) != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  [[maybe_unused]] auto* _ = dev.release();

  return ZX_OK;
}

void TiTca6408a::ConfigIn(ConfigInRequest& request, ConfigInCompleter::Sync& completer) {
  if (!IsIndexInRange(request.index())) {
    completer.Reply(fit::error(ZX_ERR_OUT_OF_RANGE));
    return;
  }

  if (request.flags() != fuchsia_hardware_gpio::GpioFlags::kNoPull) {
    completer.Reply(fit::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }

  auto result = SetBit(Register::kConfiguration, request.index());
  if (result.is_error()) {
    completer.Reply(fit::error(result.error_value()));
    return;
  }
  completer.Reply(fit::ok());
}

void TiTca6408a::ConfigOut(ConfigOutRequest& request, ConfigOutCompleter::Sync& completer) {
  if (zx_status_t status = Write(request.index(), request.initial_value()); status != ZX_OK) {
    completer.Reply(zx::error(status));
    return;
  }
  auto result = ClearBit(Register::kConfiguration, request.index());
  if (result.is_error()) {
    completer.Reply(zx::error(result.status_value()));
    return;
  }
  completer.Reply(fit::ok());
}

void TiTca6408a::SetAltFunction(SetAltFunctionRequest& request,
                                SetAltFunctionCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void TiTca6408a::SetDriveStrength(SetDriveStrengthRequest& request,
                                  SetDriveStrengthCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void TiTca6408a::GetDriveStrength(GetDriveStrengthRequest& request,
                                  GetDriveStrengthCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void TiTca6408a::Read(ReadRequest& request, ReadCompleter::Sync& completer) {
  if (!IsIndexInRange(request.index())) {
    completer.Reply(zx::error(ZX_ERR_OUT_OF_RANGE));
    return;
  }

  zx::result<uint8_t> value = ReadBit(Register::kInputPort, request.index());
  if (value.is_error()) {
    completer.Reply(zx::error(value.error_value()));
    return;
  }

  completer.Reply(fit::ok(value.value()));
}

void TiTca6408a::Write(WriteRequest& request, WriteCompleter::Sync& completer) {
  auto status = Write(request.index(), request.value());
  if (status != ZX_OK) {
    completer.Reply(fit::error(status));
    return;
  }
  completer.Reply(fit::ok());
}

zx_status_t TiTca6408a::Write(uint32_t index, uint8_t value) {
  if (!IsIndexInRange(index)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  const zx::result<> status =
      value ? SetBit(Register::kOutputPort, index) : ClearBit(Register::kOutputPort, index);
  return status.status_value();
}

void TiTca6408a::GetInterrupt(GetInterruptRequest& request,
                              GetInterruptCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void TiTca6408a::ReleaseInterrupt(ReleaseInterruptRequest& request,
                                  ReleaseInterruptCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void TiTca6408a::SetPolarity(SetPolarityRequest& request, SetPolarityCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

zx::result<uint8_t> TiTca6408a::ReadBit(Register reg, uint32_t index) {
  const auto bit = static_cast<uint8_t>(1 << (index - pin_index_offset_));
  const auto address = static_cast<uint8_t>(reg);

  uint8_t value = 0;
  auto status = i2c_.WriteReadSyncRetries(&address, sizeof(address), &value, sizeof(value),
                                          kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    zxlogf(ERROR, "Failed to read register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  return zx::ok(static_cast<uint8_t>((value & bit) ? 1 : 0));
}

zx::result<> TiTca6408a::SetBit(Register reg, uint32_t index) {
  const auto bit = static_cast<uint8_t>(1 << (index - pin_index_offset_));
  const auto address = static_cast<uint8_t>(reg);

  uint8_t value = 0;
  auto status = i2c_.WriteReadSyncRetries(&address, sizeof(address), &value, sizeof(value),
                                          kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    zxlogf(ERROR, "Failed to read register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  const uint8_t write_buf[2] = {address, static_cast<uint8_t>(value | bit)};
  status = i2c_.WriteSyncRetries(write_buf, sizeof(write_buf), kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    zxlogf(ERROR, "Failed to write register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  return zx::ok();
}

zx::result<> TiTca6408a::ClearBit(Register reg, uint32_t index) {
  const auto bit = static_cast<uint8_t>(1 << (index - pin_index_offset_));
  const auto address = static_cast<uint8_t>(reg);

  uint8_t value = 0;
  auto status = i2c_.WriteReadSyncRetries(&address, sizeof(address), &value, sizeof(value),
                                          kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    zxlogf(ERROR, "Failed to read register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  const uint8_t write_buf[2] = {address, static_cast<uint8_t>(value & ~bit)};
  status = i2c_.WriteSyncRetries(write_buf, sizeof(write_buf), kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    zxlogf(ERROR, "Failed to write register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  return zx::ok();
}

static constexpr zx_driver_ops_t ti_tca6408a_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = TiTca6408a::Create;
  return ops;
}();

}  // namespace gpio

ZIRCON_DRIVER(ti_tca6408a, gpio::ti_tca6408a_driver_ops, "zircon", "0.1");
