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
#include <kernel/mutex.h>
#include <kernel/thread.h>
#include <kernel/timer.h>
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
  // TODO(https://fxbug.dev/137029): Rationalize this state with mp active / online states.
  enum class State : uint8_t {
    Active,
    Wakeup,  // Similar to Active with respect to scheduling but signals a wakeup occurred.
    Offline,
    Suspend,
  };

  // Returns true if the power thread should be scheduled instead of eligible work because it needs
  // to perform a transition or power management function.
  bool pending_power_work() const {
    const StateMachine state = state_.load(ktl::memory_order_acquire);
    return (state.current != State::Active && state.current != State::Wakeup) ||
           state.current != state.target;
  }

  // The result of a transition request, including the status code of the request and the current
  // state when the request was made.
  struct TransitionResult {
    const zx_status_t status;
    const State starting_state;
  };

  // Requests to transition from Active to Offline.
  //
  // Returns:
  //   - {ZX_OK, Active} if the transition was successful.
  //   - {ZX_OK, Offline} if the current state was already Offline.
  //   - {ZX_ERR_BAD_STATE, current state} if the current state was not Active or Offline.
  //   - {ZX_ERR_TIMED_OUT, Active} if the current state was Active and transition timeout expired.
  TransitionResult TransitionActiveToOffline(zx_time_t timeout_at = ZX_TIME_INFINITE)
      TA_EXCL(TransitionLock::Get()) {
    Guard<Mutex> guard{TransitionLock::Get()};
    return TransitionFromTo(State::Active, State::Offline, timeout_at);
  }

  // Requests to transition from Offline to Active.
  //
  // Returns:
  //   - {ZX_OK, Offline} if the transition was successful.
  //   - {ZX_OK, Active} if the current state was already Active.
  //   - {ZX_ERR_BAD_STATE, current state} if the current state was not Active or Offline.
  //   - {ZX_ERR_TIMED_OUT, Offline} if the current state was Offline and transition timeout
  //     expired.
  TransitionResult TransitionOfflineToActive(zx_time_t timeout_at = ZX_TIME_INFINITE)
      TA_EXCL(TransitionLock::Get()) {
    Guard<Mutex> guard{TransitionLock::Get()};
    return TransitionFromTo(State::Offline, State::Active, timeout_at);
  }

  // Attempts to transition all active CPUs from Active to Suspend until the specified resume time.
  // CPUs that are Offline before suspend will remain Offline after resume until explicitly
  // transitioned to Active.
  //
  // Returns:
  //  - ZX_OK if active CPU suspend succeeded and then resumed.
  //  - ZX_ERR_TIMED_OUT when resume_at is in the past before transitioning to suspend.
  static zx_status_t TransitionAllActiveToSuspend(zx_time_t resume_at = ZX_TIME_INFINITE)
      TA_EXCL(TransitionLock::Get());

  // The result of a request to wake up the suspended or suspending boot CPU.
  enum class WakeResult : bool {
    Resumed,
    SuspendAborted,
  };

  // Transitions the boot CPU from Suspend to Wakeup to begin resuming the system or to abort an
  // incomplete suspend sequence.
  //
  // Must be called with interrupts and preempt disabled and active_cpus_suspended() == true.
  //
  // Returns:
  //  - WakeResult::Resumed if this or another wake trigger resumed the boot CPU.
  //  - WakeResult::SuspendAborted if this wake trigger occurred before suspend completed.
  //
  static WakeResult WakeBootCpu();

  // Implements the run loop executed by the CPU's idle/power thread.
  static int Run(void* arg);

  // Called by mp_unplug_current_cpu() to complete last steps of taking the CPU offline.
  void FlushAndHalt();

  // Accessors to the underlying Thread instance.
  Thread& thread() { return thread_; }
  const Thread& thread() const { return thread_; }

  // Accessor to the global active CPU suspend state.
  static bool active_cpus_suspended() {
    return active_cpus_suspended_.load(ktl::memory_order_acquire);
  }

 private:
  // Allow percpu to initialize this_cpu_;
  friend struct percpu;

  struct StateMachine {
    State current{State::Active};
    State target{State::Active};

    constexpr bool operator==(const StateMachine& other) const {
      return current == other.current && target == other.target;
    }
  };

  // Shorthand expressions for various comparisons and asserts.
  static constexpr StateMachine kActive{State::Active, State::Active};
  static constexpr StateMachine kActiveToSuspend{State::Active, State::Suspend};
  static constexpr StateMachine kWakeup{State::Wakeup, State::Wakeup};
  static constexpr StateMachine kSuspend{State::Suspend, State::Suspend};
  static constexpr StateMachine kSuspendToWakeup{State::Suspend, State::Wakeup};

  bool CompareExchangeState(StateMachine& expected, StateMachine desired) {
    return state_.compare_exchange_strong(expected, desired, ktl::memory_order_acq_rel,
                                          ktl::memory_order_acquire);
  }
  TransitionResult TransitionFromTo(State expected_state, State target_state, zx_time_t timeout_at)
      TA_REQ(TransitionLock::Get());

  ktl::atomic<StateMachine> state_{};
  AutounsignalMpUnplugEvent complete_;

  Thread thread_;
  cpu_num_t this_cpu_{INVALID_CPU};

  DECLARE_SINGLETON_MUTEX(TransitionLock);

  inline static ktl::atomic<bool> active_cpus_suspended_{false};
  inline static Timer resume_timer_ TA_GUARDED(TransitionLock::Get()){};
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_IDLE_POWER_THREAD_H_
