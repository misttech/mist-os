// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/controller-dpc.h"

#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/time.h>

#include <kernel/deadline.h>
#include <kernel/timer.h>

namespace power_management {

void ControllerDpc::NotifyPendingUpdate() {
  timer_.Cancel();
  timer_.Set(Deadline::after(ZX_TIME_INFINITE_PAST), &QueueDpc, this);
}

void ControllerDpc::QueueDpc(Timer* t, zx_time_t now, void* arg) {
  auto* controller_dpc = static_cast<ControllerDpc*>(arg);
  controller_dpc->dpc_.Queue();
}

void ControllerDpc::Invoke(Dpc* dpc) {
  auto* controller_dpc = dpc->arg<ControllerDpc>();

  auto [domain_ref, power_level_update] = controller_dpc->provider_();

  if (!power_level_update) {
    return;
  }

  if (!domain_ref->controller()) {
    return;
  }

  if (auto res = domain_ref->controller()->Post(*power_level_update); res.is_error()) {
    if (res.status_value() == ZX_ERR_SHOULD_WAIT) {
      printf(
          "Failed to post power level transition request. Port is at capacity. CPU_DRIVER possibly not servicing.\n");
    }
  }
}

}  // namespace power_management
