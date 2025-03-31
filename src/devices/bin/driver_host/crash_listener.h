// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST_CRASH_LISTENER_H_
#define SRC_DEVICES_BIN_DRIVER_HOST_CRASH_LISTENER_H_

#include <lib/async/cpp/wait.h>
#include <lib/zx/channel.h>
#include <lib/zx/exception.h>
#include <lib/zx/result.h>

#include "src/devices/bin/driver_host/driver.h"

namespace driver_host {

// Forward declare.
class DriverHost;

class CrashListener {
 public:
  explicit CrashListener(async_dispatcher_t *dispatcher, DriverHost *driver_host);

  zx::result<> Init();

  void WaitHandler(async_dispatcher_t *dispatcher, async::WaitBase *wait, zx_status_t status,
                   const zx_packet_signal_t *signal);

  void ReadException();

  uint32_t ProcessException(zx_exception_info_t info, zx::exception &exception);

  std::optional<fuchsia_driver_host::DriverCrashInfo> TakeByTid(zx_koid_t tid);

 private:
  async_dispatcher_t *dispatcher_;

  zx::channel exception_channel_;
  std::optional<async::WaitMethod<CrashListener, &CrashListener::WaitHandler>> wait_;

  DriverHost *driver_host_;
  std::unordered_map<zx_koid_t, fuchsia_driver_host::DriverCrashInfo> entries_;
};

}  // namespace driver_host

#endif  // SRC_DEVICES_BIN_DRIVER_HOST_CRASH_LISTENER_H_
