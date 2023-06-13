// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clock.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/fdf/dispatcher.h>
#include <zircon/types.h>

#include <memory>

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
  clock_impl_protocol_t clock_proto;
  auto status = device_get_protocol(parent, ZX_PROTOCOL_CLOCK_IMPL, &clock_proto);
  if (status != ZX_OK) {
    zxlogf(ERROR, "device_get_protocol failed %d", status);
    return status;
  }

  auto clock_ids = ddk::GetMetadataArray<clock_id_t>(parent, DEVICE_METADATA_CLOCK_IDS);
  if (!clock_ids.is_ok()) {
    zxlogf(ERROR, "GetMetadataArray failed %d.", clock_ids.error_value());
    return clock_ids.error_value();
  }

  for (auto clock : *clock_ids) {
    auto clock_id = clock.clock_id;
    fbl::AllocChecker ac;
    std::unique_ptr<ClockDevice> dev(new (&ac) ClockDevice(parent, &clock_proto, clock_id));
    if (!ac.check()) {
      zxlogf(ERROR, "Failed to allocate clock device.");
      return ZX_ERR_NO_MEMORY;
    }

    char name[20];
    snprintf(name, sizeof(name), "clock-%u", clock_id);
    zx_device_prop_t props[] = {
        {BIND_CLOCK_ID, 0, clock_id},
    };

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      zxlogf(ERROR, "Failed to create IO directory endpoints - %s.", endpoints.status_string());
      return endpoints.status_value();
    }

    status = dev->ServeOutgoing(std::move(endpoints->server));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to serve outgoing directory: %s", zx_status_get_string(status));
      return status;
    }

    std::array offers = {
        fuchsia_hardware_clock::Service::Name,
    };

    status = dev->DdkAdd(ddk::DeviceAddArgs(name)
                             .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                             .set_props(props)
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

namespace {

constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = ClockDevice::Create;
  return ops;
}();

}  // namespace

ZIRCON_DRIVER(clock, driver_ops, "zircon", "0.1");
