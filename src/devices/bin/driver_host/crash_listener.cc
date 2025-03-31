// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/crash_listener.h"

#include <lib/fdf/env.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include "src/devices/bin/driver_host/driver_host.h"
#include "src/devices/lib/log/log.h"

namespace driver_host {

CrashListener::CrashListener(async_dispatcher_t *dispatcher, DriverHost *driver_host)
    : dispatcher_(dispatcher), driver_host_(driver_host) {}

zx::result<> CrashListener::Init() {
  zx_status_t status = zx::process::self()->create_exception_channel(0, &exception_channel_);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  wait_.emplace(this, exception_channel_.get(), ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED);
  status = wait_->Begin(dispatcher_);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::make_result(status);
}

void CrashListener::WaitHandler(async_dispatcher_t *dispatcher, async::WaitBase *wait,
                                zx_status_t status, const zx_packet_signal_t *signal) {
  if (status != ZX_OK) {
    return;
  }

  if (signal->observed & ZX_CHANNEL_READABLE) {
    ReadException();
    wait_->Begin(dispatcher_);
  } else {
    exception_channel_.reset();
    wait_.reset();
  }
}

void CrashListener::ReadException() {
  // Read the exception info and handle from the channel.
  zx_exception_info_t info;
  zx_handle_t exception_handle;

  zx_status_t status =
      exception_channel_.read(0, &info, &exception_handle, sizeof(info), 1, nullptr, nullptr);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to read from exception channel: %s", zx_status_get_string(status));
    return;
  }

  zx::exception exception(exception_handle);

  uint32_t state = ProcessException(info, exception);
  if (state) {
    status = exception.set_property(ZX_PROP_EXCEPTION_STATE, &state, sizeof(state));
    if (status != ZX_OK) {
      LOGF(ERROR, "Failed to set exception state: %s", zx_status_get_string(status));
    }
  }

  // Dropping the zx::exception closes it so processing can continue.
}

uint32_t CrashListener::ProcessException(zx_exception_info_t info, zx::exception &exception) {
  const void *out_driver;
  zx_status_t status = fdf_env_get_driver_on_tid(info.tid, &out_driver);

  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to get driver info from tid.");
    return ZX_EXCEPTION_STATE_TRY_NEXT;
  }

  std::optional<const Driver *> validated_driver = driver_host_->ValidateAndGetDriver(out_driver);
  if (!validated_driver) {
    LOGF(ERROR, "Failed to validate driver with the driver host.");
    return ZX_EXCEPTION_STATE_TRY_NEXT;
  }

  entries_[info.tid] = fuchsia_driver_host::DriverCrashInfo{{
      .url = validated_driver.value()->url(),
      .node_token = validated_driver.value()->node_token(),
  }};
  LOGF(ERROR, "Driver crashed in driver host: Driver url '%s'",
       validated_driver.value()->url().c_str());
  return ZX_EXCEPTION_STATE_TRY_NEXT;
}

std::optional<fuchsia_driver_host::DriverCrashInfo> CrashListener::TakeByTid(zx_koid_t tid) {
  auto it = entries_.find(tid);
  if (it == entries_.end()) {
    return std::nullopt;
  }
  fuchsia_driver_host::DriverCrashInfo entry = std::move(it->second);
  entries_.erase(it);
  return entry;
}

}  // namespace driver_host
