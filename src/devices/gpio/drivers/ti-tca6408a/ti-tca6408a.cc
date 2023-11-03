// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ti-tca6408a.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/hardware/gpioimpl/cpp/bind.h>

namespace {

// Arbitrary values for I2C retries.
constexpr uint8_t kI2cRetries = 10;
constexpr zx::duration kI2cRetryDelay = zx::usec(1);

zx::result<uint32_t> ParseMetadata(
    const fidl::VectorView<fuchsia_driver_compat::wire::Metadata>& metadata) {
  for (const auto& m : metadata) {
    if (m.type == DEVICE_METADATA_PRIVATE) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size %s", zx_status_get_string(status));
        continue;
      }

      if (size != sizeof(uint32_t)) {
        FDF_LOG(ERROR, "Unexpected metadata size: got %zu, expected %zu", size, sizeof(uint32_t));
        continue;
      }

      uint32_t data;
      status = m.data.read(&data, 0, sizeof(data));
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read %s", zx_status_get_string(status));
        continue;
      }

      return zx::ok(data);
    }
  }

  FDF_LOG(ERROR, "Failed to find matching metadata!");
  return zx::error(ZX_ERR_NOT_FOUND);
}

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

  // Get metadata.
  uint32_t pin_index_offset = 0;
  {
    zx::result result = incoming()->Connect<fuchsia_driver_compat::Service::Device>("pdev");
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open pdev service: %s", result.status_string());
      return result.take_error();
    }
    auto compat = fidl::WireSyncClient(std::move(result.value()));
    if (!compat.is_valid()) {
      FDF_LOG(ERROR, "Failed to get compat");
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto metadata = compat->GetMetadata();
    if (!metadata.ok()) {
      FDF_LOG(ERROR, "Failed to GetMetadata %s", metadata.FormatDescription().data());
      return zx::error(metadata.status());
    }
    if (metadata->is_error()) {
      FDF_LOG(ERROR, "Failed to GetMetadata %s", zx_status_get_string(metadata->error_value()));
      return metadata->take_error();
    }

    auto vals = ParseMetadata(metadata.value()->metadata);
    if (vals.is_error()) {
      FDF_LOG(ERROR, "Failed to ParseMetadata %s", zx_status_get_string(vals.error_value()));
      return vals.take_error();
    }
    pin_index_offset = vals.value();

    auto serve_result = ServeMetadata(compat, metadata.value()->metadata);
    if (serve_result.is_error()) {
      FDF_LOG(ERROR, "Failed to ServeMetadata %s",
              zx_status_get_string(serve_result.error_value()));
      return serve_result.take_error();
    }
  }

  device_ = std::make_unique<TiTca6408a>(std::move(i2c), pin_index_offset);

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

zx::result<> TiTca6408aDevice::ServeMetadata(
    fidl::WireSyncClient<fuchsia_driver_compat::Device>& compat,
    const fidl::VectorView<fuchsia_driver_compat::wire::Metadata>& metadata) {
  // Start compat_server_.
  fidl::WireResult topo = compat->GetTopologicalPath();
  if (!topo.ok()) {
    FDF_LOG(WARNING, "GetTopologicalPath failed: %s", topo.FormatDescription().c_str());
    return zx::error(topo.error().status());
  }

  auto node_name_val = node_name().value_or("NA");

  std::string topological_path(topo.value().path.get());
  topological_path += "/";
  topological_path += node_name_val;
  compat_server_.Init("default", topological_path);

  // Serve Metadata.
  for (auto& meta : metadata) {
    uint64_t meta_size;
    auto status = meta.data.get_property(ZX_PROP_VMO_CONTENT_SIZE, &meta_size, sizeof(meta_size));
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Could not read ZX_PROP_VMO_CONTENT_SIZE, st = %s",
              zx_status_get_string(status));
      return zx::error(status);
    }

    zx_vaddr_t buffer;
    status = zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0,
                         meta.data.get(), 0, meta_size, &buffer);

    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Unable to map metadata vmo, st = %d\n", status);
      continue;
    }

    // Add metadata the same way as in the root_driver.
    compat_server_.AddMetadata(meta.type, reinterpret_cast<void*>(buffer), meta_size);

    status = zx_vmar_unmap(zx_vmar_root_self(), buffer, meta_size);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Unable to unmap metadata vmo, st = %d\n", status);
    }
  }

  zx_status_t status = compat_server_.Serve(dispatcher(), outgoing().get());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Could not start compat server service. st = %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> TiTca6408aDevice::CreateNode() {
  fidl::Arena arena;
  auto offers = compat_server_.CreateOffers(arena);
  offers.push_back(
      fdf::MakeOffer<fuchsia_hardware_gpioimpl::Service>(arena, component::kDefaultInstance));
  auto properties =
      std::vector{fdf::MakeProperty(arena, bind_fuchsia_hardware_gpioimpl::SERVICE,
                                    bind_fuchsia_hardware_gpioimpl::SERVICE_DRIVERTRANSPORT)};

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDeviceName)
                  .offers(arena, std::move(offers))
                  .properties(arena, std::move(properties))
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                controller_endpoints.status_string());

  fidl::WireResult result =
      fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
    return zx::error(result.status());
  }
  controller_.Bind(std::move(controller_endpoints->client));

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

zx::result<uint8_t> TiTca6408a::ReadBit(Register reg, uint32_t index) {
  const auto bit = static_cast<uint8_t>(1 << (index - pin_index_offset_));
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
  const auto bit = static_cast<uint8_t>(1 << (index - pin_index_offset_));
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
  const auto bit = static_cast<uint8_t>(1 << (index - pin_index_offset_));
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
