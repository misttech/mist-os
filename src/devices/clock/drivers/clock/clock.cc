// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clock.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
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
  zxlogf(ERROR, "Unexpected Clock FIDL call: 0x%lx", metadata.method_ordinal);
}

zx_status_t ClockDevice::ServeOutgoing(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  fuchsia_hardware_clock::Service::InstanceHandler handler({
      .clock = bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure),
  });

  zx::result result = outgoing_.AddService<fuchsia_hardware_clock::Service>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  result = outgoing_.Serve(std::move(server_end));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve the outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  return ZX_OK;
}

void ClockDevice::DdkRelease() { delete this; }

zx_status_t ClockDevice::Create(void* ctx, zx_device_t* parent) {
  {
    zx::result clock_impl = ClockImplProxy::Create(parent);
    if (clock_impl.is_error()) {
      return clock_impl.status_value();
    }

    // Process init metadata while we are still the exclusive owner of the clock client.
    ClockInitDevice::Create(parent, clock_impl.value());
  }

  auto clock_ids = ddk::GetMetadataArray<clock_id_t>(parent, DEVICE_METADATA_CLOCK_IDS);
  if (!clock_ids.is_ok()) {
    zxlogf(ERROR, "GetMetadataArray failed %d.", clock_ids.error_value());
    return clock_ids.error_value();
  }

  for (auto clock : *clock_ids) {
    zx::result clock_impl = ClockImplProxy::Create(parent);
    if (clock_impl.is_error()) {
      return clock_impl.status_value();
    }

    auto clock_id = clock.clock_id;
    fbl::AllocChecker ac;
    std::unique_ptr<ClockDevice> dev(
        new (&ac) ClockDevice(parent, std::move(clock_impl.value()), clock_id));
    if (!ac.check()) {
      zxlogf(ERROR, "Failed to allocate clock device.");
      return ZX_ERR_NO_MEMORY;
    }

    char name[20];
    snprintf(name, sizeof(name), "clock-%u", clock_id);
    zx_device_str_prop_t props[] = {
        ddk::MakeStrProperty(bind_fuchsia::CLOCK_ID, clock_id),
    };

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      zxlogf(ERROR, "Failed to create IO directory endpoints - %s.", endpoints.status_string());
      return endpoints.status_value();
    }

    zx_status_t status = dev->ServeOutgoing(std::move(endpoints->server));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to serve outgoing directory: %s", zx_status_get_string(status));
      return status;
    }

    std::array offers = {
        fuchsia_hardware_clock::Service::Name,
    };

    status = dev->DdkAdd(ddk::DeviceAddArgs(name)
                             .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                             .set_str_props(props)
                             .set_fidl_service_offers(offers)
                             .set_outgoing_dir(endpoints->client.TakeChannel()));
    if (status != ZX_OK) {
      zxlogf(ERROR, "DdkAdd failed - %s", zx_status_get_string(status));
      return status;
    }

    // dev is now owned by devmgr.
    [[maybe_unused]] auto ptr = dev.release();
  }

  return ZX_OK;
}

void ClockInitDevice::Create(zx_device_t* parent, const ClockImplProxy& clock) {
  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_clockimpl::wire::InitMetadata>(
      parent, DEVICE_METADATA_CLOCK_INIT);
  if (!decoded.is_ok()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No init metadata provided");
    } else {
      zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    }
    return;
  }

  auto device = std::make_unique<ClockInitDevice>(parent);
  if (device->ConfigureClocks(*decoded.value(), clock) != ZX_OK) {
    // Return without adding the init device if some clocks could not be configured. This will
    // prevent all drivers that depend on the initial state from binding, which should make it more
    // obvious that something has gone wrong.
    return;
  }

  zx_device_str_prop_t props[] = {
      ddk::MakeStrProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
  };

  zx_status_t status = device->DdkAdd(ddk::DeviceAddArgs("clock-init")
                                          .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                                          .set_str_props(props));
  if (status == ZX_OK) {
    [[maybe_unused]] auto _ = device.release();
  } else {
    zxlogf(ERROR, "Failed to add clock-init: %s", zx_status_get_string(status));
  }
}

zx_status_t ClockInitDevice::ConfigureClocks(
    const fuchsia_hardware_clockimpl::wire::InitMetadata& metadata, const ClockImplProxy& clock) {
  // Stop processing the list if any call returns an error so that clocks are not accidentally
  // enabled in an unknown state.
  for (const auto& step : metadata.steps) {
    if (!step.has_call()) {
      zxlogf(ERROR, "Clock Metadata init step is missing a call field");
      return ZX_ERR_INVALID_ARGS;
    }

    // Delay doesn't apply to any particular clock ID so we enforce that the ID field is
    // unset. Every other type of init call requires an ID so we enforce that ID is set.
    if (step.call().is_delay() && step.has_id()) {
      zxlogf(ERROR, "Clock Init Delay calls must not have an ID, id = %u", step.id());
      return ZX_ERR_INVALID_ARGS;
    }
    if (!step.call().is_delay() && !step.has_id()) {
      zxlogf(ERROR, "Clock init calls must have an ID");
      return ZX_ERR_INVALID_ARGS;
    }

    if (step.call().is_enable()) {
      if (zx_status_t status = clock.Enable(step.id()); status != ZX_OK) {
        zxlogf(ERROR, "Enable() failed for %u: %s", step.id(), zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_disable()) {
      if (zx_status_t status = clock.Disable(step.id()); status != ZX_OK) {
        zxlogf(ERROR, "Disable() failed for %u: %s", step.id(), zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_rate_hz()) {
      if (zx_status_t status = clock.SetRate(step.id(), step.call().rate_hz()); status != ZX_OK) {
        zxlogf(ERROR, "SetRate(%lu) failed for %u: %s", step.call().rate_hz(), step.id(),
               zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_input_idx()) {
      if (zx_status_t status = clock.SetInput(step.id(), step.call().input_idx());
          status != ZX_OK) {
        zxlogf(ERROR, "SetInput(%u) failed for %u: %s", step.call().input_idx(), step.id(),
               zx_status_get_string(status));
        return status;
      }
    } else if (step.call().is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.call().delay())));
    }
  }

  return ZX_OK;
}

zx::result<ClockImplProxy> ClockImplProxy::Create(zx_device_t* parent) {
  const ddk::ClockImplProtocolClient clock_banjo(parent);
  if (clock_banjo.is_valid()) {
    zxlogf(DEBUG, "Using Banjo clockimpl protocol");
    return zx::ok(ClockImplProxy{clock_banjo});
  }

  zx::result clock_fidl =
      ddk::Device<void>::DdkConnectRuntimeProtocol<fuchsia_hardware_clockimpl::Service::Device>(
          parent);
  if (clock_fidl.is_ok()) {
    zxlogf(DEBUG, "Failed to get Banjo clockimpl protocol, falling back to FIDL");
    return zx::ok(ClockImplProxy{std::move(clock_fidl.value())});
  }

  zxlogf(ERROR, "Failed to get Banjo or FIDL clockimpl protocol");
  return zx::error(ZX_ERR_NO_RESOURCES);
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

namespace {

constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = ClockDevice::Create;
  return ops;
}();

}  // namespace

ZIRCON_DRIVER(clock, driver_ops, "zircon", "0.1");
