// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_SCHED_INCLUDE_LIB_SCHED_THREAD_BASE_H_
#define ZIRCON_KERNEL_LIB_SCHED_INCLUDE_LIB_SCHED_THREAD_BASE_H_

#include <zircon/assert.h>
#include <zircon/time.h>

#include <fbl/intrusive_wavl_tree.h>
#include <ffl/fixed.h>

namespace sched {

// Forward-declared; defined in <lib/sched/run-queue.h>
template <typename Thread>
class RunQueue;

// Fixed-point wrappers for cleaner time arithmetic when dealing with other
// fixed-point quantities.
using Duration = ffl::Fixed<zx_duration_t, 0>;
using Time = ffl::Fixed<zx_time_t, 0>;

// The parameters that specify a thread's activation period (i.e., the
// recurring cycle in which it is scheduled).
struct BandwidthParameters {
  // The duration of the thread's activation period.
  Duration period;

  // The duration within a given activation period in which a thread is expected
  // to complete its *required* work.
  Duration firm_capacity;
};

// The base thread class from which we expect schedulable thread types to
// inherit (following the 'Curiously Recurring Template Pattern'). This class
// manages the scheduler-related thread state of interest to this library.
template <typename Thread>
class ThreadBase {
 public:
  constexpr ThreadBase(BandwidthParameters bandwidth, Time start)
      : period_(bandwidth.period), firm_capacity_(bandwidth.firm_capacity) {
    ZX_ASSERT(bandwidth.period >= bandwidth.firm_capacity);
    ZX_ASSERT(bandwidth.firm_capacity > 0);
    Reactivate(start);
  }

  constexpr Duration period() const { return period_; }

  constexpr Duration firm_capacity() const { return firm_capacity_; }

  // The start of the thread's current activation period.
  constexpr Time start() const { return start_; }

  // The end of the thread's current activation period.
  constexpr Time finish() const { return start_ + period_; }

  // The cumulative amount of scheduled time within the current activation
  // period.
  constexpr Duration time_slice_used() const { return time_slice_used_; }

  // Reactivates the thread in a new activation period beginning `now`.
  constexpr void Reactivate(Time now) {
    start_ = now;
    time_slice_used_ = Duration{0};
  }

  // Accounts for the time in which the thread was executed (as measured outside
  // this library).
  constexpr void Tick(Duration elapsed) { time_slice_used_ += elapsed; }

  // Whether the thread is currently queued to be scheduled (in a RunQueue).
  constexpr bool IsQueued() const { return run_queue_.node.InContainer(); }

 private:
  // For access to `run_queue_` alone.
  friend class RunQueue<Thread>;

  Duration period_{0};
  Duration firm_capacity_{0};
  Time start_{Time::Min()};
  Duration time_slice_used_{0};

  // Encapsulates the state specific to the run queue.
  struct RunQueueState {
    fbl::WAVLTreeNodeState<Thread*> node;

    // Minimum finish time of all the descendants of this node in the run queue.
    // See RunQueue<Thread>::SubtreeMinFinishObserverTraits for the management
    // and utility of this quantity.
    Time subtree_min_finish{Time::Min()};
  } run_queue_;
};

}  // namespace sched

#endif  // ZIRCON_KERNEL_LIB_SCHED_INCLUDE_LIB_SCHED_THREAD_BASE_H_
