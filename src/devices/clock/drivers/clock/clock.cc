// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clock.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fdf/dispatcher.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <ddk/metadata/clock.h>
#include <fbl/alloc_checker.h>

void ClockDevice::Enable(EnableCompleter::Sync& completer) {
  completer.Reply(zx::make_result(clock_.Enable(id_)));
}

void ClockDevice::Disable(DisableCompleter::Sync& completer) {
  completer.Reply(zx::make_result(clock_.Disable(id_)));
}

void ClockDevice::IsEnabled(IsEnabledCompleter::Sync& completer) {
  bool enabled;
  zx_status_t status = clock_.IsEnabled(id_, &enabled);
  if (status == ZX_OK) {
    completer.ReplySuccess(enabled);
  } else {
    completer.ReplyError(status);
  }
}

void ClockDevice::SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) {
  completer.Reply(zx::make_result(clock_.SetRate(id_, request->hz)));
}

void ClockDevice::ClockDevice::QuerySupportedRate(QuerySupportedRateRequestView request,
                                                  QuerySupportedRateCompleter::Sync& completer) {
  uint64_t hz_out;
  zx_status_t status = clock_.QuerySupportedRate(id_, request->hz_in, &hz_out);
  if (status == ZX_OK) {
    completer.ReplySuccess(hz_out);
  } else {
    completer.ReplyError(status);
  }
}

void ClockDevice::GetRate(GetRateCompleter::Sync& completer) {
  uint64_t current_rate;
  zx_status_t status = clock_.GetRate(id_, &current_rate);
  if (status == ZX_OK) {
    completer.ReplySuccess(current_rate);
  } else {
    completer.ReplyError(status);
  }
}

void ClockDevice::SetInput(SetInputRequestView request, SetInputCompleter::Sync& completer) {
  completer.Reply(zx::make_result(clock_.SetInput(id_, request->idx)));
}

void ClockDevice::GetNumInputs(GetNumInputsCompleter::Sync& completer) {
  uint32_t num_inputs;
  zx_status_t status = clock_.GetNumInputs(id_, &num_inputs);
  if (status == ZX_OK) {
    completer.ReplySuccess(num_inputs);
  } else {
    completer.ReplyError(status);
  }
}

void ClockDevice::GetInput(GetInputCompleter::Sync& completer) {
  uint32_t input;
  zx_status_t status = clock_.GetInput(id_, &input);
  if (status == ZX_OK) {
    completer.ReplySuccess(input);
  } else {
    completer.ReplyError(status);
  }
}

void ClockDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_clock::Clock> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unexpected Clock FIDL call: 0x%lx", metadata.method_ordinal);
}

zx_status_t ClockDevice::Init(const std::shared_ptr<fdf::Namespace>& incoming,
                              const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                              const std::optional<std::string>& node_name,
                              AddChildCallback add_child_callback) {
  char child_node_name[20];
  snprintf(child_node_name, sizeof(child_node_name), "clock-%u", id_);

  zx::result compat_result =
      compat_server_.Initialize(incoming, outgoing, node_name, child_node_name);
  if (compat_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize compat server: %s", compat_result.status_string());
    return compat_result.status_value();
  }

  auto node_offers = compat_server_.CreateOffers2();
  node_offers.emplace_back(fdf::MakeOffer2<fuchsia_hardware_clock::Service>(child_node_name));

  const std::vector<fuchsia_driver_framework::NodeProperty> node_properties{
      fdf::MakeProperty(bind_fuchsia::CLOCK_ID, id_)};

  zx_status_t status = add_child_callback(child_node_name, node_properties, node_offers);
  if (status != ZX_OK) {
    return status;
  }

  fuchsia_hardware_clock::Service::InstanceHandler instance_handler{
      {.clock = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                        fidl::kIgnoreBindingClosure)}};
  zx::result result = outgoing->AddService<fuchsia_hardware_clock::Service>(
      std::move(instance_handler), child_node_name);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add clock service to outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  return ZX_OK;
}

zx::result<> ClockDriver::Start() {
  zx::result clock_impl = ClockImplProxy::Create(incoming());
  if (clock_impl.is_error()) {
    return clock_impl.take_error();
  }

  fidl::Arena arena;
  zx::result metadata = compat::GetMetadata<fuchsia_hardware_clockimpl::wire::InitMetadata>(
      incoming(), arena, DEVICE_METADATA_CLOCK_INIT);
  if (metadata.is_ok()) {
    zx_status_t status = ConfigureClocks(*metadata.value().get(), clock_impl.value());
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to configure clocks: %s", zx_status_get_string(status));
      return zx::error(status);
    }

    const std::vector<fuchsia_driver_framework::NodeProperty> node_properties{
        fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK)};

    zx::result node = AddChild("clock-init", node_properties, {});
    if (node.is_error()) {
      FDF_LOG(ERROR, "Failed to create child node: %s", node.status_string());
      return node.take_error();
    }
    clock_init_child_node_ = std::move(node.value());
  } else if (metadata.status_value() == ZX_ERR_NOT_FOUND) {
    FDF_LOG(INFO, "No init metadata provided");
  } else {
    FDF_LOG(ERROR, "Failed to get metadata: %s", metadata.status_string());
    return metadata.take_error();
  }

  zx_status_t status = CreateClockDevices();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create clock devices: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx_status_t ClockDriver::CreateClockDevices() {
  zx::result clock_ids =
      compat::GetMetadataArray<clock_id_t>(incoming(), DEVICE_METADATA_CLOCK_IDS);
  if (clock_ids.is_error()) {
    FDF_LOG(ERROR, "Failed to get clock ID's: %s", clock_ids.status_string());
    return clock_ids.status_value();
  }

  for (auto clock : *clock_ids) {
    zx::result clock_impl = ClockImplProxy::Create(incoming());
    if (clock_impl.is_error()) {
      return clock_impl.status_value();
    }

    // ClockDevice must be dynamically allocated because it has a ServerBindingGroup and compat
    // server property which cannot be moved.
    auto clock_device =
        std::make_unique<ClockDevice>(clock.clock_id, std::move(clock_impl.value()));

    zx_status_t status = clock_device->Init(
        incoming(), outgoing(), node_name(),
        [this](std::string_view child_node_name,
               const fuchsia_driver_framework::NodePropertyVector& node_properties,
               const std::vector<fuchsia_driver_framework::Offer>& node_offers) {
          zx::result node = AddChild(child_node_name, node_properties, node_offers);
          if (node.is_error()) {
            FDF_LOG(ERROR, "Failed to create child node: %s", node.status_string());
            return node.status_value();
          }

          return ZX_OK;
        });
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to initialize clock device: %s", zx_status_get_string(status));
      return status;
    }

    clock_devices_.emplace_back(std::move(clock_device));
  }

  return ZX_OK;
}

zx_status_t ClockDriver::ConfigureClocks(
    const fuchsia_hardware_clockimpl::wire::InitMetadata& metadata, const ClockImplProxy& clock) {
  // Stop processing the list if any call returns an error so that clocks are not accidentally
  // enabled in an unknown state.
  for (const auto& step : metadata.steps) {
    if (!step.has_call()) {
      FDF_LOG(ERROR, "Clock Metadata init step is missing a call field");
      return ZX_ERR_INVALID_ARGS;
    }

    // Delay doesn't apply to any particular clock ID so we enforce that the ID field is
    // unset. Every other type of init call requires an ID so we enforce that ID is set.
    if (step.call().is_delay() && step.has_id()) {
      FDF_LOG(ERROR, "Clock Init Delay calls must not have an ID, id = %u", step.id());
      return ZX_ERR_INVALID_ARGS;
    }
    if (!step.call().is_delay() && !step.has_id()) {
      FDF_LOG(ERROR, "Clock init calls must have an ID");
      return ZX_ERR_INVALID_ARGS;
    }

    if (step.call().is_enable()) {
      if (zx_status_t status = clock.Enable(step.id()); status != ZX_OK) {
        FDF_LOG(ERROR, "Enable() failed for %u: %s", step.id(), zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_disable()) {
      if (zx_status_t status = clock.Disable(step.id()); status != ZX_OK) {
        FDF_LOG(ERROR, "Disable() failed for %u: %s", step.id(), zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_rate_hz()) {
      if (zx_status_t status = clock.SetRate(step.id(), step.call().rate_hz()); status != ZX_OK) {
        FDF_LOG(ERROR, "SetRate(%lu) failed for %u: %s", step.call().rate_hz(), step.id(),
                zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_input_idx()) {
      if (zx_status_t status = clock.SetInput(step.id(), step.call().input_idx());
          status != ZX_OK) {
        FDF_LOG(ERROR, "SetInput(%u) failed for %u: %s", step.call().input_idx(), step.id(),
                zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.call().delay())));
    }
  }

  return ZX_OK;
}

zx::result<ClockImplProxy> ClockImplProxy::Create(const std::shared_ptr<fdf::Namespace>& incoming) {
  zx::result<ddk::ClockImplProtocolClient> clock_banjo =
      compat::ConnectBanjo<ddk::ClockImplProtocolClient>(incoming);
  if (clock_banjo.is_ok() && clock_banjo->is_valid()) {
    FDF_LOG(DEBUG, "Using Banjo clockimpl protocol");
    return zx::ok(ClockImplProxy{clock_banjo.value()});
  }

  zx::result clock_fidl = incoming->Connect<fuchsia_hardware_clockimpl::Service::Device>();
  if (clock_fidl.is_error()) {
    FDF_LOG(ERROR, "Failed to get Banjo or FIDL clockimpl protocol");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  FDF_LOG(DEBUG, "Failed to get Banjo clockimpl protocol, falling back to FIDL");
  return zx::ok(ClockImplProxy{std::move(clock_fidl.value())});
}

zx_status_t ClockImplProxy::Enable(uint32_t id) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.Enable(id);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->Enable(id);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t ClockImplProxy::Disable(uint32_t id) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.Disable(id);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->Disable(id);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t ClockImplProxy::IsEnabled(uint32_t id, bool* out_enabled) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.IsEnabled(id, out_enabled);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->IsEnabled(id);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_enabled = result->value()->enabled;
  return ZX_OK;
}

zx_status_t ClockImplProxy::SetRate(uint32_t id, uint64_t hz) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.SetRate(id, hz);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->SetRate(id, hz);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t ClockImplProxy::QuerySupportedRate(uint32_t id, uint64_t hz, uint64_t* out_hz) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.QuerySupportedRate(id, hz, out_hz);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->QuerySupportedRate(id, hz);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_hz = result->value()->hz;
  return ZX_OK;
}

zx_status_t ClockImplProxy::GetRate(uint32_t id, uint64_t* out_hz) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.GetRate(id, out_hz);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->GetRate(id);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_hz = result->value()->hz;
  return ZX_OK;
}

zx_status_t ClockImplProxy::SetInput(uint32_t id, uint32_t idx) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.SetInput(id, idx);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->SetInput(id, idx);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t ClockImplProxy::GetNumInputs(uint32_t id, uint32_t* out_n) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.GetNumInputs(id, out_n);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->GetNumInputs(id);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_n = result->value()->n;
  return ZX_OK;
}

zx_status_t ClockImplProxy::GetInput(uint32_t id, uint32_t* out_index) const {
  if (clock_banjo_.is_valid()) {
    return clock_banjo_.GetInput(id, out_index);
  }

  fdf::Arena arena('CLK_');
  const auto result = clock_fidl_.buffer(arena)->GetInput(id);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_index = result->value()->index;
  return ZX_OK;
}

FUCHSIA_DRIVER_EXPORT(ClockDriver);
