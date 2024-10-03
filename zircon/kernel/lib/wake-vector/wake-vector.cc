// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/affine/ratio.h>
#include <lib/wake-vector.h>
#include <zircon/types.h>

#include <kernel/idle_power_thread.h>
#include <kernel/thread.h>

namespace wake_vector {

WakeResult WakeEvent::Trigger() {
  DEBUG_ASSERT(arch_ints_disabled());
  DEBUG_ASSERT(Thread::Current::Get()->preemption_state().PreemptIsEnabled() == false);
  if (const PendingState state = pending_state_.load(); !state.pending()) {
    pending_state_.store({true, current_boot_ticks()});
    return IdlePowerThread::TriggerSystemWakeEvent();
  }
  return WakeResult::BadState;
}

void WakeEvent::Acknowledge() {
  if (const PendingState state = pending_state_.load(); state.pending()) {
    pending_state_.store({false, state.last_triggered_boot_ticks()});
    IdlePowerThread::AcknowledgeSystemWakeEvent();
  }
}

zx_instant_boot_t WakeEvent::PendingState::last_triggered_boot_time() const {
  return timer_get_ticks_to_time_ratio().Scale(last_triggered_boot_ticks());
}

void WakeEvent::Dump(FILE* f, zx_instant_boot_t log_triggered_after_boot_time) {
  Guard<SpinLock, IrqSave> guard{WakeEventListLock::Get()};

  for (const IdlePowerThread::WakeEvent& event : list_) {
    const PendingState state = event.pending_state_.load();
    if (state.pending() || state.last_triggered_boot_time() >= log_triggered_after_boot_time) {
      WakeVector::Diagnostics diagnostics;
      event.wake_vector_.GetDiagnostics(diagnostics);

      if (diagnostics.enabled) {
        fprintf(f, "  koid: %6" PRIu64 ", pending: %d, last triggered: %" PRIi64 ", extra: %.*s\n",
                diagnostics.koid, state.pending(), state.last_triggered_boot_time(),
                static_cast<int>(diagnostics.extra.size()), diagnostics.extra.data());
      }
    }
  }
}

}  // namespace wake_vector
