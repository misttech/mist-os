// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/driver/v2/child-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace driver_transport {

zx::result<> ChildTransportDriver::Start() {
  auto connect_result = incoming()->Connect<fuchsia_hardware_i2cimpl::Service::Device>();
  if (connect_result.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect fuchsia.hardware.i2cimpl device protocol.",
             KV("status", connect_result.status_string()));
    return connect_result.take_error();
  }

  auto result = QueryParent(std::move(connect_result.value()));
  if (result.is_error()) {
    return result.take_error();
  }

  zx::result child_result = AddChild("transport-child", {}, {});
  if (child_result.is_error()) {
    return child_result.take_error();
  }

  controller_.Bind(std::move(child_result.value()), dispatcher());

  // Since we set the dispatcher to "ALLOW_SYNC_CALLS" in the driver CML, we
  // need to seal the option after we finish all our sync calls over driver transport.
  auto status =
      fdf_dispatcher_seal(driver_dispatcher()->get(), FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS);
  if (status != ZX_OK) {
    FDF_SLOG(ERROR, "Failed to seal ALLOW_SYNC_CALLS.", KV("status", zx_status_get_string(status)));
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> ChildTransportDriver::QueryParent(
    fdf::ClientEnd<fuchsia_hardware_i2cimpl::Device> client_end) {
  fdf::Arena arena('GIZM');

  // Query and store the hardware ID.
  auto max_transfer_sz_result = fdf::WireCall(client_end).buffer(arena)->GetMaxTransferSize();
  if (!max_transfer_sz_result.ok()) {
    FDF_SLOG(ERROR, "Failed to request max transfer size.",
             KV("status", max_transfer_sz_result.status_string()));
    return zx::error(max_transfer_sz_result.status());
  }
  if (max_transfer_sz_result->is_error()) {
    FDF_SLOG(ERROR, "Hardware ID request returned an error.",
             KV("status", max_transfer_sz_result->error_value()));
    return max_transfer_sz_result->take_error();
  }

  max_transfer_size_ = max_transfer_sz_result.value()->size;
  FDF_LOG(INFO, "Max transfer size: %zu", max_transfer_size_);

  // Set the bitrate.
  constexpr uint32_t kBitrate = 5;
  auto bitrate_result = fdf::WireCall(client_end).buffer(arena)->SetBitrate(kBitrate);
  if (!bitrate_result.ok()) {
    FDF_SLOG(ERROR, "Failed to set the bitrate.", KV("status", bitrate_result.status_string()));
    return zx::error(bitrate_result.status());
  }
  if (bitrate_result->is_error()) {
    FDF_SLOG(ERROR, "Bitrate request returned an error.",
             KV("status", bitrate_result->error_value()));
    return bitrate_result->take_error();
  }
  FDF_LOG(INFO, "Successfully set the bitrate to %u", kBitrate);

  return zx::ok();
}

}  // namespace driver_transport

FUCHSIA_DRIVER_EXPORT(driver_transport::ChildTransportDriver);
