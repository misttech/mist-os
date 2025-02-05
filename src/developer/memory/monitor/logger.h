// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_MEMORY_MONITOR_LOGGER_H_
#define SRC_DEVELOPER_MEMORY_MONITOR_LOGGER_H_

#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>

#include "src/developer/memory/metrics/capture.h"
#include "src/developer/memory/metrics/digest.h"
#include "src/developer/memory/monitor/high_water.h"
#include "src/developer/memory/monitor/memory_monitor_config.h"
#include "src/developer/memory/pressure_signaler/pressure_observer.h"
#include "src/lib/fxl/macros.h"

namespace monitor {

class Logger {
 public:
  using CaptureCb = fit::function<zx_status_t(memory::Capture*)>;
  using DigestCb = fit::function<void(const memory::Capture&, memory::Digest*)>;

  Logger(async_dispatcher_t* dispatcher, HighWater* high_water, CaptureCb capture_cb,
         DigestCb digest_cb, memory_monitor_config::Config* config)
      : dispatcher_(dispatcher),
        high_water_(high_water),
        capture_cb_(std::move(capture_cb)),
        digest_cb_(std::move(digest_cb)),
        config_(config) {}

  // SetPressureLevel needs to be called at least once for the Logger to start.
  void SetPressureLevel(pressure_signaler::Level l);

 private:
  void Log();
  async_dispatcher_t* dispatcher_;
  HighWater* high_water_;
  CaptureCb capture_cb_;
  DigestCb digest_cb_;
  memory_monitor_config::Config* config_;
  bool capture_high_water_ = false;
  zx::duration duration_;
  async::TaskClosureMethod<Logger, &Logger::Log> task_{this};

  FXL_DISALLOW_COPY_AND_ASSIGN(Logger);
};

}  // namespace monitor

#endif  // SRC_DEVELOPER_MEMORY_MONITOR_LOGGER_H_
