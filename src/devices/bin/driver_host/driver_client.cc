// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/driver_client.h"

#include "src/devices/bin/driver_host/driver.h"
#include "src/devices/lib/log/log.h"

namespace dfv2 {

DriverClient::DriverClient(fbl::RefPtr<Driver> driver, std::string_view url)
    : driver_(std::move(driver)), url_(url) {}

void DriverClient::Bind(fdf::ClientEnd<fuchsia_driver_framework::Driver> client) {
  driver_client_.Bind(std::move(client), fdf_dispatcher_get_current_dispatcher(), this);
}

void DriverClient::Start(fuchsia_driver_framework::DriverStartArgs start_args,
                         fit::callback<void(zx::result<>)> callback) {
  fdf::Arena arena('DSTT');
  driver_client_.buffer(arena)
      ->Start(fidl::ToWire(arena, std::move(start_args)))
      .Then([this, cb = std::move(callback)](
                fdf::WireUnownedResult<fuchsia_driver_framework::Driver::Start>& result) mutable {
        if (!result.ok()) {
          LOGF(WARNING, "Failed to start driver: %s", result.FormatDescription().c_str());
          start_status_ = result.error().status();
          cb(zx::error(result.error().status()));
        } else if (result.value().is_error()) {
          LOGF(WARNING, "Failed to start driver: %s",
               zx_status_get_string(result.value().error_value()));
          start_status_ = result.value().error_value();
          cb(zx::error(result.value().error_value()));
        } else {
          start_status_ = ZX_OK;
          cb(zx::ok());
        }
      });
}

void DriverClient::Stop() {
  if (!start_status_) {
    LOGF(WARNING, "Driver %s Stop() hook is executed before start is completed", url_.c_str());
  } else if (start_status_.value() != ZX_OK) {
    LOGF(WARNING, "Driver %s Stop() hook is executed after start failed with status %s",
         url_.c_str(), zx_status_get_string(start_status_.value()));
  }

  fdf::Arena arena('DSTP');
  fidl::OneWayStatus stop_status = driver_client_.buffer(arena)->Stop();
  if (!stop_status.ok()) {
    LOGF(ERROR, "Failed to Stop driver '%s', %s.", url_.c_str(),
         stop_status.FormatDescription().c_str());
    return;
  }
}

void DriverClient::on_fidl_error(fidl::UnbindInfo error) {
  LOGF(INFO, "Driver server for '%s' closed: '%s'.", url_.c_str(),
       error.FormatDescription().c_str());
  driver_->Unbind();
  driver_->ShutdownClient();
}

void DriverClient::handle_unknown_event(
    fidl::UnknownEventMetadata<fuchsia_driver_framework::Driver> metadata) {
  LOGF(WARNING, "Driver client for '%s' received unknown event: event_ordinal(%lu)", url_.c_str(),
       metadata.event_ordinal);
}

}  // namespace dfv2
