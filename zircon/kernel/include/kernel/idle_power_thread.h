// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_IDLE_POWER_THREAD_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_IDLE_POWER_THREAD_H_

#include <stdint.h>
#include <zircon/time.h>

#include <arch/mp_unplug_event.h>
#include <kernel/thread.h>
#include <ktl/atomic.h>

//
// Platform Independent Per-CPU Idle/Power Thread
//
// Manages the idle, suspend, and offline functions of a CPU. This thread is scheduled whenever
// there is no eligible work in the run queues of a CPU or a power state management operation is
// requested for the CPU. The idle/power thread provides the platform-independent run loop for
// handling these functions and delegates to the platform-specific subsystems for the relevant
// operations.
//

class IdlePowerThread final {
 public:
  IdlePowerThread() = default;
  ~IdlePowerThread() = default;

  IdlePowerThread(const IdlePowerThread&) = delete;
  IdlePowerThread& operator=(const IdlePowerThread&) = delete;
  IdlePowerThread(IdlePowerThread&&) = delete;
  IdlePowerThread& operator=(IdlePowerThread&&) = delete;

  // The current or target high-level operational state of the CPU. The depth and type of
  // idle/suspend states depends inputs from other subsystems, such as timer and run queues and
  // system power targets.
  enum class State : uint8_t {
    Online,
    Offline,
    Suspend,
  };

  // Returns true if the power thread should be scheduled instead of eligible work because it needs
  // to perform a transition or power management function.
  bool pending_power_work() const {
    const StateMachine state = state_.load(ktl::memory_order_acquire);
    return state.current != State::Online || state.current != state.target;
  }

  // The result of a transition request, including the status code of the request and the current
  // state when the request was made.
  struct TransitionResult {
    const zx_status_t status;
    const State starting_state;
  };

  // Requests to reansition from Online to Offline.
  //
  // Returns:
  //   - {ZX_OK, Online} if the transition was successful.
  //   - {ZX_OK, Offline} if the current state was already Offline.
  //   - {ZX_ERR_BAD_STATE, current state} if the current state was not Online or Offline.
  //   - {ZX_ERR_TIMED_OUT, Online} if the current state was Online and transition timeout expired.
  TransitionResult TransitionToOffline(zx_time_t timeout_at = ZX_TIME_INFINITE) {
    return TransitionFromTo(State::Online, State::Offline, timeout_at);
  }

  // Requests to reansition from Offline to Online.
  //
  // Returns:
  //   - {ZX_OK, Offline} if the transition was successful.
  //   - {ZX_OK, Online} if the current state was already Online.
  //   - {ZX_ERR_BAD_STATE, current state} if the current state was not Online or Offline.
  //   - {ZX_ERR_TIMED_OUT, Offline} if the current state was Offline and transition timeout
  //     expired.
  TransitionResult TransitionToOnline(zx_time_t timeout_at = ZX_TIME_INFINITE) {
    return TransitionFromTo(State::Offline, State::Online, timeout_at);
  }

  // Implements the run loop executed by the CPU's idle/power thread.
  static int Run(void* arg);

  // Called by mp_unplug_current_cpu() to complete last steps of taking the CPU offline.
  void FlushAndHalt();

  Thread& thread() { return thread_; }
  const Thread& thread() const { return thread_; }

 private:
  // Allow percpu to initialize this_cpu_;
  friend struct percpu;

  TransitionResult TransitionFromTo(State expected_state, State target_state, zx_time_t timeout_at);

  struct StateMachine {
    State current{State::Online};
    State target{State::Online};
  };
  ktl::atomic<StateMachine> state_{};
  AutounsignalMpUnplugEvent complete_;

  Thread thread_;
  cpu_num_t this_cpu_;
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_IDLE_POWER_THREAD_H_
