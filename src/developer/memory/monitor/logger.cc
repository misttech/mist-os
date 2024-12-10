// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/monitor/logger.h"

#include <lib/syslog/cpp/macros.h>

#include "lib/zx/time.h"
#include "src/developer/memory/metrics/printer.h"

namespace monitor {

void Logger::SetPressureLevel(pressure_signaler::Level l) {
  switch (l) {
    case pressure_signaler::kImminentOOM:
      duration_ = zx::sec(config_->imminent_oom_capture_delay_s());
      break;
    case pressure_signaler::kCritical:
      duration_ = zx::sec(config_->critical_capture_delay_s());
      break;
    case pressure_signaler::kWarning:
      duration_ = zx::sec(config_->warning_capture_delay_s());
      break;
    case pressure_signaler::kNormal:
      duration_ = zx::sec(config_->normal_capture_delay_s());
      break;
    case pressure_signaler::kNumLevels:
      break;
  }
  if (config_->capture_on_pressure_change()) {
    task_.Cancel();
    task_.PostDelayed(dispatcher_, zx::usec(1));
  } else if (task_.last_deadline() > zx::deadline_after(duration_)) {
    task_.Cancel();
    task_.PostDelayed(dispatcher_, duration_);
  }
}

void Logger::Log() {
  memory::Capture c;
  auto s = capture_cb_(&c);
  if (s != ZX_OK) {
    FX_LOGS_FIRST_N(INFO, 1) << "Error getting Capture: " << s;
    return;
  }
  memory::Digest d;
  digest_cb_(c, &d);
  std::ostringstream oss;
  memory::TextPrinter p(oss);

  p.PrintDigest(d);
  auto str = std::move(oss).str();
  std::ranges::replace(str, '\n', ' ');
  FX_LOGS(INFO) << str;

  task_.PostDelayed(dispatcher_, duration_);
}

}  // namespace monitor
