// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ti-tca6408a.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

namespace {

// Arbitrary values for I2C retries.
constexpr uint8_t kI2cRetries = 10;
constexpr zx::duration kI2cRetryDelay = zx::usec(1);

}  // namespace

namespace gpio {

zx::result<> TiTca6408aDevice::Start() {
  // Get I2C.
  ddk::I2cChannel i2c;
  {
    zx::result result = incoming()->Connect<fuchsia_hardware_i2c::Service::Device>("i2c");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open i2c service: %s", result.status_string());
      return result.take_error();
    }
    i2c = std::move(result.value());

    // Clear the polarity inversion register.
    const uint8_t write_buf[2] = {static_cast<uint8_t>(TiTca6408a::Register::kPolarityInversion),
                                  0};
    i2c.WriteSyncRetries(write_buf, sizeof(write_buf), kI2cRetries, kI2cRetryDelay);
  }

  ZX_ASSERT(compat_server_
                .Initialize(incoming(), outgoing(), node_name(), kDeviceName,
                            compat::ForwardMetadata::All())
                .is_ok());

  device_ = std::make_unique<TiTca6408a>(std::move(i2c));

  auto result = outgoing()->AddService<fuchsia_hardware_gpioimpl::Service>(
      fuchsia_hardware_gpioimpl::Service::InstanceHandler({
          .device = bindings_.CreateHandler(device_.get(), fdf::Dispatcher::GetCurrent()->get(),
                                            fidl::kIgnoreBindingClosure),
      }));
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
    return result.take_error();
  }

  return CreateNode();
}

void TiTca6408aDevice::Stop() {
  auto status = controller_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove child: %s", status.status_string());
  }
}

zx::result<> TiTca6408aDevice::CreateNode() {
  fidl::Arena arena;
  auto offers = compat_server_.CreateOffers2(arena);
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_gpioimpl::Service>(arena, component::kDefaultInstance));

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .offers2(arena, std::move(offers))
                  .Build();

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  fidl::WireResult result =
      fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints.server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints.client));

  return zx::ok();
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

void TiTca6408a::GetPins(GetPinsCompleter::Sync& completer) {}

void TiTca6408a::GetInitSteps(GetInitStepsCompleter::Sync& completer) {}

void TiTca6408a::GetControllerId(GetControllerIdCompleter::Sync& completer) { completer.Reply(0); }

zx::result<uint8_t> TiTca6408a::ReadBit(Register reg, uint32_t index) {
  const auto bit = static_cast<uint8_t>(1 << index);
  const auto address = static_cast<uint8_t>(reg);

  uint8_t value = 0;
  auto status = i2c_.WriteReadSyncRetries(&address, sizeof(address), &value, sizeof(value),
                                          kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to read register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  return zx::ok(static_cast<uint8_t>((value & bit) ? 1 : 0));
}

zx::result<> TiTca6408a::SetBit(Register reg, uint32_t index) {
  const auto bit = static_cast<uint8_t>(1 << index);
  const auto address = static_cast<uint8_t>(reg);

  uint8_t value = 0;
  auto status = i2c_.WriteReadSyncRetries(&address, sizeof(address), &value, sizeof(value),
                                          kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to read register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  const uint8_t write_buf[2] = {address, static_cast<uint8_t>(value | bit)};
  status = i2c_.WriteSyncRetries(write_buf, sizeof(write_buf), kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to write register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  return zx::ok();
}

zx::result<> TiTca6408a::ClearBit(Register reg, uint32_t index) {
  const auto bit = static_cast<uint8_t>(1 << index);
  const auto address = static_cast<uint8_t>(reg);

  uint8_t value = 0;
  auto status = i2c_.WriteReadSyncRetries(&address, sizeof(address), &value, sizeof(value),
                                          kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to read register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  const uint8_t write_buf[2] = {address, static_cast<uint8_t>(value & ~bit)};
  status = i2c_.WriteSyncRetries(write_buf, sizeof(write_buf), kI2cRetries, kI2cRetryDelay);
  if (status.status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to write register %u: %s", address, zx_status_get_string(status.status));
    return zx::error(status.status);
  }

  return zx::ok();
}

}  // namespace gpio

FUCHSIA_DRIVER_EXPORT(gpio::TiTca6408aDevice);
