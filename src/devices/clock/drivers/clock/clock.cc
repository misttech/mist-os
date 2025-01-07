// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clock.h"

#include <fidl/fuchsia.hardware.clockimpl/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/metadata/cpp/metadata.h>
#include <lib/fdf/dispatcher.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <fbl/alloc_checker.h>

void ClockDevice::Enable(EnableCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->Enable(id_);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send Enable request to clock %u: %s", id_, result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to enable clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess();
}

void ClockDevice::Disable(DisableCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->Disable(id_);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send Disable request to clock %u: %s", id_, result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to disable clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess();
}

void ClockDevice::IsEnabled(IsEnabledCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->IsEnabled(id_);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send IsEnabled request to clock %u: %s", id_, result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to check if clock %u is enabled: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess(result.value()->enabled);
}

void ClockDevice::SetRate(SetRateRequestView request, SetRateCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->SetRate(id_, request->hz);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send SetRate request to clock %u: %s", id_, result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to set rate for clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess();
}

void ClockDevice::ClockDevice::QuerySupportedRate(QuerySupportedRateRequestView request,
                                                  QuerySupportedRateCompleter::Sync& completer) {
  fdf::WireUnownedResult result =
      clock_impl_.buffer(arena_)->QuerySupportedRate(id_, request->hz_in);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send QuerySupportedRate request to clock %u: %s", id_,
            result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to query supported rate for clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess(result.value()->hz);
}

void ClockDevice::GetRate(GetRateCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->GetRate(id_);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send GetRate request to clock %u: %s", id_, result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to get rate rate for clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess(result.value()->hz);
}

void ClockDevice::SetInput(SetInputRequestView request, SetInputCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->SetInput(id_, request->idx);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send SetInput request to clock %u: %s", id_, result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to set input for clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess();
}

void ClockDevice::GetNumInputs(GetNumInputsCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->GetNumInputs(id_);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send GetNumInputs request to clock %u: %s", id_,
            result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to get number of inputs for clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess(result.value()->n);
}

void ClockDevice::GetInput(GetInputCompleter::Sync& completer) {
  fdf::WireUnownedResult result = clock_impl_.buffer(arena_)->GetInput(id_);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send GetInput request to clock %u: %s", id_, result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Failed to get input for clock %u: %s", id_,
            zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  completer.ReplySuccess(result.value()->index);
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
  zx::result clock_impl = incoming->Connect<fuchsia_hardware_clockimpl::Service::Device>();
  if (clock_impl.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to the clock-impl FIDL protocol: %s",
            clock_impl.status_string());
    return clock_impl.status_value();
  }
  clock_impl_.Bind(std::move(clock_impl.value()));

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
  zx::result clock_impl = incoming()->Connect<fuchsia_hardware_clockimpl::Service::Device>();
  if (clock_impl.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to the clock-impl FIDL protocol: %s",
            clock_impl.status_string());
    return clock_impl.take_error();
  }

  fidl::Arena arena;
  zx::result metadata = compat::GetMetadata<fuchsia_hardware_clockimpl::wire::InitMetadata>(
      incoming(), arena, DEVICE_METADATA_CLOCK_INIT);
  if (metadata.is_ok()) {
    zx_status_t status = ConfigureClocks(*metadata.value().get(), std::move(clock_impl.value()));
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
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  zx::result clock_ids_metadata =
      fdf_metadata::GetMetadata<fuchsia_hardware_clockimpl::ClockIdsMetadata>(incoming());
  if (clock_ids_metadata.is_error()) {
    FDF_LOG(ERROR, "Failed to get clock IDs: %s", clock_ids_metadata.status_string());
    return clock_ids_metadata.status_value();
  }

  const auto& clock_ids = clock_ids_metadata.value().clock_ids();
  if (!clock_ids.has_value()) {
    return ZX_OK;
  }
  for (auto clock_id : clock_ids.value()) {
    // ClockDevice must be dynamically allocated because it has a ServerBindingGroup and compat
    // server property which cannot be moved.
    auto clock_device = std::make_unique<ClockDevice>(clock_id);
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
#else
  static_assert(false,
                "Cannot create clock devices: Clock IDs not available at given Fuchsia API level");
#endif
}

zx_status_t ClockDriver::ConfigureClocks(
    const fuchsia_hardware_clockimpl::wire::InitMetadata& metadata,
    fdf::ClientEnd<fuchsia_hardware_clockimpl::ClockImpl> clock_impl_client) {
  fdf::WireSyncClient<fuchsia_hardware_clockimpl::ClockImpl> clock_impl{
      std::move(clock_impl_client)};
  fdf::Arena arena{'CLOC'};

  // Stop processing the list if any call returns an error so that clocks are not accidentally
  // enabled in an unknown state.
  for (const auto& step : metadata.steps) {
    if (!step.has_call()) {
      FDF_LOG(ERROR, "Clock Metadata init step is missing a call field");
      return ZX_ERR_INVALID_ARGS;
    }
    auto call = step.call();

    // Delay doesn't apply to any particular clock ID so we enforce that the ID field is
    // unset. Every other type of init call requires an ID so we enforce that ID is set.
    if (call.is_delay() && step.has_id()) {
      FDF_LOG(ERROR, "Clock Init Delay calls must not have an ID, id = %u", step.id());
      return ZX_ERR_INVALID_ARGS;
    }
    if (!call.is_delay() && !step.has_id()) {
      FDF_LOG(ERROR, "Clock init calls must have an ID");
      return ZX_ERR_INVALID_ARGS;
    }

    auto clock_id = step.id();
    if (call.is_enable()) {
      fdf::WireUnownedResult result = clock_impl.buffer(arena)->Enable(clock_id);
      if (!result.ok()) {
        FDF_LOG(ERROR, "Failed to send Enable request for clock %u: %s", clock_id,
                result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "Failed to enable clock %u: %s", clock_id,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (call.is_disable()) {
      fdf::WireUnownedResult result = clock_impl.buffer(arena)->Disable(clock_id);
      if (!result.ok()) {
        FDF_LOG(ERROR, "Failed to send Disable request for clock %u: %s", clock_id,
                result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "Failed to disable clock %u: %s", clock_id,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (call.is_rate_hz()) {
      fdf::WireUnownedResult result = clock_impl.buffer(arena)->SetRate(clock_id, call.rate_hz());
      if (!result.ok()) {
        FDF_LOG(ERROR, "Failed to send SetRate request for clock %u: %s", clock_id,
                result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "Failed to set rate for clock %u: %s", clock_id,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (call.is_input_idx()) {
      fdf::WireUnownedResult result =
          clock_impl.buffer(arena)->SetInput(clock_id, call.input_idx());
      if (!result.ok()) {
        FDF_LOG(ERROR, "Failed to send SetInput request for clock %u: %s", clock_id,
                result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "Failed to set input for clock %u: %s", clock_id,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (call.is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(call.delay())));
    }
  }

  return ZX_OK;
}

FUCHSIA_DRIVER_EXPORT(ClockDriver);
