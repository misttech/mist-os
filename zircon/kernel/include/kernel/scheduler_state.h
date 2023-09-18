// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_STATE_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_STATE_H_

#include <lib/zircon-internal/macros.h>
#include <stddef.h>
#include <stdint.h>
#include <zircon/syscalls/scheduler.h>
#include <zircon/types.h>

#include <fbl/enum_bits.h>
#include <fbl/intrusive_wavl_tree.h>
#include <ffl/fixed.h>
#include <kernel/cpu.h>
#include <kernel/spinlock.h>
#include <kernel/thread_lock.h>
#include <ktl/limits.h>
#include <ktl/move.h>
#include <ktl/pair.h>

// Forward declarations.
struct Thread;
namespace unittest {
class ThreadEffectiveProfileObserver;
}

namespace internal {
template <typename T>
class PiNodeAdapter;
}  // namespace internal

#ifndef SCHEDULER_EXTRA_INVARIANT_VALIDATION
#define SCHEDULER_EXTRA_INVARIANT_VALIDATION false
#endif

inline constexpr bool kSchedulerExtraInvariantValidation = SCHEDULER_EXTRA_INVARIANT_VALIDATION;

enum thread_state : uint8_t {
  THREAD_INITIAL = 0,
  THREAD_READY,
  THREAD_RUNNING,
  THREAD_BLOCKED,
  THREAD_BLOCKED_READ_LOCK,
  THREAD_SLEEPING,
  THREAD_SUSPENDED,
  THREAD_DEATH,
};

// Fixed-point task weight.
//
// The 16bit fractional component accommodates the exponential curve defining
// the priority-to-weight relation:
//
//      Weight = 1.225^(Priority - 31)
//
// This yields roughly 10% bandwidth difference between adjacent priorities.
//
// Weights should not be negative, however, the value is signed for consistency
// with zx_time_t (SchedTime) and zx_duration_t (SchedDuration), which are the
// primary types used in conjunction with SchedWeight. This is to make it less
// likely that expressions involving weights are accidentally promoted to
// unsigned.
using SchedWeight = ffl::Fixed<int64_t, 16>;

// Fixed-point time slice remainder.
//
// The 20bit fractional component represents a fractional time slice with a
// precision of ~1us.
using SchedRemainder = ffl::Fixed<int64_t, 20>;

// Fixed-point utilization factor. Represents the ratio between capacity and
// period or capacity and relative deadline, depending on which type of
// utilization is being evaluated.
//
// The 20bit fractional component represents the utilization with a precision
// of ~1us.
using SchedUtilization = ffl::Fixed<int64_t, 20>;

// Fixed-point types wrapping time and duration types to make time expressions
// cleaner in the scheduler code.
using SchedDuration = ffl::Fixed<zx_duration_t, 0>;
using SchedTime = ffl::Fixed<zx_time_t, 0>;

namespace internal {
// Conversion table entry. Scales the integer argument to a fixed-point weight
// in the interval (0.0, 1.0].
struct WeightTableEntry {
  constexpr WeightTableEntry(int64_t value)
      : value{ffl::FromRatio<int64_t>(value, SchedWeight::Format::Power)} {}
  constexpr operator SchedWeight() const { return value; }
  const SchedWeight value;
};

// Table of fixed-point constants converting from kernel priority to fair
// scheduler weight.
inline constexpr WeightTableEntry kPriorityToWeightTable[] = {
    121,   149,   182,   223,   273,   335,   410,   503,   616,   754,  924,
    1132,  1386,  1698,  2080,  2549,  3122,  3825,  4685,  5739,  7030, 8612,
    10550, 12924, 15832, 19394, 23757, 29103, 35651, 43672, 53499, 65536};
}  // namespace internal

// Represents the key deadline scheduler parameters using fixed-point types.
// This is a fixed point version of the ABI type zx_sched_deadline_params_t that
// makes expressions in the scheduler logic less verbose.
struct SchedDeadlineParams {
  SchedDuration capacity_ns{0};
  SchedDuration deadline_ns{0};
  SchedUtilization utilization{0};

  constexpr SchedDeadlineParams() = default;
  constexpr SchedDeadlineParams(SchedDuration capacity_ns, SchedDuration deadline_ns)
      : capacity_ns{capacity_ns},
        deadline_ns{deadline_ns},
        utilization{capacity_ns / deadline_ns} {}

  constexpr SchedDeadlineParams(SchedUtilization utilization, SchedDuration deadline_ns)
      : capacity_ns{deadline_ns * utilization},
        deadline_ns{deadline_ns},
        utilization{utilization} {}

  constexpr SchedDeadlineParams(const SchedDeadlineParams&) = default;
  constexpr SchedDeadlineParams& operator=(const SchedDeadlineParams&) = default;

  constexpr SchedDeadlineParams(const zx_sched_deadline_params_t& params)
      : capacity_ns{params.capacity},
        deadline_ns{params.relative_deadline},
        utilization{capacity_ns / deadline_ns} {}
  constexpr SchedDeadlineParams& operator=(const zx_sched_deadline_params_t& params) {
    *this = SchedDeadlineParams{params};
    return *this;
  }

  friend bool operator==(SchedDeadlineParams a, SchedDeadlineParams b) {
    return a.capacity_ns == b.capacity_ns && a.deadline_ns == b.deadline_ns;
  }
  friend bool operator!=(SchedDeadlineParams a, SchedDeadlineParams b) { return !(a == b); }
};

// Utilities that return fixed-point Expression representing the given integer
// time units in terms of system time units (nanoseconds).
template <typename T>
constexpr auto SchedNs(T nanoseconds) {
  return ffl::FromInteger(ZX_NSEC(nanoseconds));
}
template <typename T>
constexpr auto SchedUs(T microseconds) {
  return ffl::FromInteger(ZX_USEC(microseconds));
}
template <typename T>
constexpr auto SchedMs(T milliseconds) {
  return ffl::FromInteger(ZX_MSEC(milliseconds));
}

// Specifies the type of scheduling algorithm applied to a thread.
enum class SchedDiscipline {
  Fair,
  Deadline,
};

// Per-thread state used by the unified version of Scheduler.
class SchedulerState {
 public:
  // The key type of this node operated on by WAVLTree.
  using KeyType = ktl::pair<SchedTime, uint64_t>;

  struct BaseProfile {
    BaseProfile() : fair{} {}

    explicit BaseProfile(int priority, bool inheritable = true);
    explicit BaseProfile(SchedWeight weight, bool inheritable = true)
        : inheritable{inheritable}, fair{.weight{weight}} {}
    explicit BaseProfile(SchedDeadlineParams deadline_params)
        : discipline{SchedDiscipline::Deadline},
          inheritable{true},  // Deadline profiles are always inheritable.
          deadline{deadline_params} {}

    bool IsFair() const { return discipline == SchedDiscipline::Fair; }
    bool IsDeadline() const { return discipline == SchedDiscipline::Deadline; }

    SchedDiscipline discipline{SchedDiscipline::Fair};
    bool inheritable{true};

    union {
      struct {
        SchedWeight weight{0};
      } fair;
      SchedDeadlineParams deadline;
    };
  };

  enum class ProfileDirtyFlag {
    Clean = 0,
    BaseDirty = 1,
    InheritedDirty = 2,
  };

  // TODO(johngro) 2022-09-21:
  //
  // This odd pattern requires some explanation.  Typically, we would use full
  // specialization in order to control at compile time whether or not
  // dirty-tracking was enabled. So, we would have:
  //
  // ```
  // template <bool Enable>
  // class Tracker { // disabled tracker impl };
  //
  // template <>
  // class Tracker<true> { // enabled tracker impl };
  // ```
  //
  // Unfortunately, there is a bug in GCC which prevents us from doing this in
  // the logical way.  Specifically, GCC does not currently allow full
  // specialization of classes declared in class/struct scope, even though this
  // should be supported as of C++17 (which the kernel is currently using).
  //
  // The bug writeup is here:
  // https://gcc.gnu.org/bugzilla/show_bug.cgi?id=85282
  //
  // It has been confirmed as a real bug, but it has been open for over 4 years
  // now.  The most recent update was about 6 months ago, and it basically said
  // "well, the fix is not going to make it into GCC 12".
  //
  // So, we are using the workaround suggested in the bug's discussion during
  // the most recent update.  Instead of using full specialization as we
  // normally would, we use an odd form of partial specialization instead, which
  // basically boils down to the same thing.
  //
  // If/when GCC finally fixes this, we can come back here and fix this.
  //
  template <bool EnableDirtyTracking, typename = void>
  class EffectiveProfileDirtyTracker {
   public:
    static inline constexpr bool kDirtyTrackingEnabled = false;
    void MarkBaseProfileChanged() {}
    void MarkInheritedProfileChanged() {}
    void Clean() {}
    void AssertDirtyState(ProfileDirtyFlag) const {}
    void AssertDirty() const {}
    ProfileDirtyFlag dirty_flags() const { return ProfileDirtyFlag::Clean; }
  };

  template <bool EnableDirtyTracking>
  class EffectiveProfileDirtyTracker<EnableDirtyTracking,
                                     std::enable_if_t<EnableDirtyTracking == true>> {
   public:
    static inline constexpr bool kDirtyTrackingEnabled = true;
    inline void MarkBaseProfileChanged();
    inline void MarkInheritedProfileChanged();
    void Clean() { dirty_flags_ = ProfileDirtyFlag::Clean; }
    void AssertDirtyState(ProfileDirtyFlag expected) const {
      ASSERT_MSG(expected == dirty_flags_, "Expected %u, Observed %u",
                 static_cast<uint32_t>(expected), static_cast<uint32_t>(dirty_flags_));
    }
    void AssertDirty() const {
      ASSERT_MSG(ProfileDirtyFlag::Clean != dirty_flags_, "Expected != 0, Observed %u",
                 static_cast<uint32_t>(dirty_flags_));
    }
    ProfileDirtyFlag dirty_flags() const { return dirty_flags_; }

   private:
    ProfileDirtyFlag dirty_flags_{ProfileDirtyFlag::Clean};
  };

  struct EffectiveProfile
      : public EffectiveProfileDirtyTracker<kSchedulerExtraInvariantValidation> {
    EffectiveProfile() : fair{} {}
    explicit EffectiveProfile(const BaseProfile& base_profile) : fair{} {
      if (base_profile.discipline == SchedDiscipline::Fair) {
        ZX_DEBUG_ASSERT(discipline == SchedDiscipline::Fair);
        fair.weight = base_profile.fair.weight;
      } else {
        ZX_DEBUG_ASSERT(base_profile.discipline == SchedDiscipline::Deadline);
        discipline = SchedDiscipline::Deadline;
        deadline = base_profile.deadline;
      }
    }

    bool IsFair() const { return discipline == SchedDiscipline::Fair; }
    bool IsDeadline() const { return discipline == SchedDiscipline::Deadline; }

    // The scheduling discipline of this profile. Determines whether the thread
    // is enqueued on the fair or deadline run queues and whether the weight or
    // deadline parameters are used.
    SchedDiscipline discipline{SchedDiscipline::Fair};

    // The current fair or deadline parameters of the profile.
    union {
      struct {
        SchedWeight weight{0};
        SchedRemainder normalized_timeslice_remainder{0};
      } fair;
      SchedDeadlineParams deadline;
    };
  };

  // Values stored in the SchedulerState of Thread instances which tracks the
  // aggregate profile values inherited from upstream contributors.
  struct InheritedProfileValues {
    // Inherited from fair threads.
    SchedWeight total_weight{0};

    // Inherited from deadline threads.
    SchedUtilization uncapped_utilization{0};
    SchedDuration min_deadline{SchedDuration::Max()};
  };

  struct WaitQueueInheritedSchedulerState {
   public:
    WaitQueueInheritedSchedulerState() = default;
    ~WaitQueueInheritedSchedulerState() { AssertIsReset(); }

    void Reset() { new (this) WaitQueueInheritedSchedulerState{}; }

    void AssertIsReset() const {
      if constexpr (kSchedulerExtraInvariantValidation) {
        ASSERT(ipvs.total_weight == SchedWeight{0});
        ASSERT(ipvs.uncapped_utilization == SchedUtilization{0});
        ASSERT(ipvs.min_deadline == SchedDuration::Max());
        ASSERT(start_time == SchedTime{0});
        ASSERT(finish_time == SchedTime{0});
        ASSERT(time_slice_ns == SchedTime{0});
        ASSERT(time_slice_used_ns == SchedTime{0});
      }
    }

    InheritedProfileValues ipvs{};
    SchedTime start_time{0};  // TODO(johngro): Do we need this?
    SchedTime finish_time{0};
    SchedDuration time_slice_ns{0};
    SchedDuration time_slice_used_ns{0};
  };

  // Converts from kernel priority value in the interval [0, 31] to weight in
  // the interval (0.0, 1.0]. See the definition of SchedWeight for an
  // explanation of the weight distribution.
  static constexpr SchedWeight ConvertPriorityToWeight(int priority) {
    return internal::kPriorityToWeightTable[priority];
  }

  SchedulerState() {}
  explicit SchedulerState(const SchedulerState::BaseProfile& base_profile)
      : base_profile_(base_profile), effective_profile_(base_profile) {}

  SchedulerState(const SchedulerState&) = delete;
  SchedulerState& operator=(const SchedulerState&) = delete;

  // Returns the effective mask of CPUs a thread may run on, based on the
  // thread's affinity masks and CPUs currently active on the system.
  cpu_mask_t GetEffectiveCpuMask(cpu_mask_t active_mask) const {
    // The thread may run on any active CPU allowed by both its hard and
    // soft CPU affinity.
    const cpu_mask_t available_mask = active_mask & soft_affinity_ & hard_affinity_;

    // Return the mask honoring soft affinity if it is viable, otherwise ignore
    // soft affinity and honor only hard affinity.
    if (likely(available_mask != 0)) {
      return available_mask;
    }

    return active_mask & hard_affinity_;
  }

  void RecomputeEffectiveProfile() TA_REQ(thread_lock);

  EffectiveProfile SnapshotEffectiveProfileLocked() const TA_REQ(thread_lock) {
    return effective_profile_;
  }
  EffectiveProfile SnapshotEffectiveProfile() const TA_EXCL(thread_lock) {
    Guard<MonitoredSpinLock, IrqSave> thread_lock_guard{ThreadLock::Get(), SOURCE_TAG};
    return SnapshotEffectiveProfileLocked();
  }

  BaseProfile SnapshotBaseProfileLocked() const TA_REQ(thread_lock) { return base_profile_; }
  BaseProfile SnapshotBaseProfile() const TA_EXCL(thread_lock) {
    Guard<MonitoredSpinLock, IrqSave> thread_lock_guard{ThreadLock::Get(), SOURCE_TAG};
    return SnapshotBaseProfileLocked();
  }

  // Returns the current effective profile for this thread.
  const EffectiveProfile& effective_profile() const { return effective_profile_; }

  // Returns the type of scheduling discipline for this thread.
  SchedDiscipline discipline() const TA_REQ(thread_lock) { return effective_profile_.discipline; }

  // Returns the key used to order the run queue.
  KeyType key() const { return {start_time_, generation_}; }

  // Returns the generation count from the last time the thread was enqueued
  // in the runnable tree.
  uint64_t generation() const { return generation_; }
  uint64_t flow_id() const { return flow_id_; }

  zx_time_t last_started_running() const { return last_started_running_.raw_value(); }
  zx_duration_t runtime_ns() const { return runtime_ns_.raw_value(); }

  SchedDuration expected_runtime_ns() const { return expected_runtime_ns_; }
  SchedDuration time_slice_ns() const { return time_slice_ns_; }
  SchedDuration time_slice_used_ns() const { return time_slice_used_ns_; }
  SchedDuration remaining_time_slice_ns() const { return time_slice_ns_ - time_slice_used_ns_; }

  SchedTime start_time() const { return start_time_; }
  SchedTime finish_time() const { return finish_time_; }

  cpu_mask_t hard_affinity() const { return hard_affinity_; }
  cpu_mask_t soft_affinity() const { return soft_affinity_; }

  int32_t weight() const TA_REQ(thread_lock) {
    return discipline() == SchedDiscipline::Fair
               ? static_cast<int32_t>(effective_profile_.fair.weight.raw_value())
               : ktl::numeric_limits<int32_t>::max();
  }

  cpu_num_t curr_cpu() const { return curr_cpu_; }
  cpu_num_t last_cpu() const { return last_cpu_; }

  thread_state state() const { return state_; }
  void set_state(thread_state state) { state_ = state; }

  void set_next_cpu(cpu_num_t next_cpu) { next_cpu_ = next_cpu; }

  Thread* previous_thread() const { return previous_thread_; }

 private:
  friend class Scheduler;
  friend class OwnedWaitQueue;
  friend class WaitQueue;
  friend class WaitQueueCollection;
  template <typename T>
  friend class internal::PiNodeAdapter;

  // Allow tests to observe/modify our state.
  friend class LoadBalancerTest;
  friend struct WaitQueueOrderingTests;
  friend class unittest::ThreadEffectiveProfileObserver;

  // TODO(eieio): Remove these once all of the members accessed by Thread are
  // moved to accessors.
  friend Thread;
  friend void thread_construct_first(Thread*, const char*);
  friend void dump_thread_locked(Thread*, bool);

  // Returns true of the task state is currently enqueued in the run queue.
  bool InQueue() const { return run_queue_node_.InContainer(); }

  // Returns true if the task is active (queued or running) on a run queue.
  bool active() const { return active_; }

  // Sets the task state to active (on a run queue). Returns true if the task
  // was not previously active.
  bool OnInsert() {
    const bool was_active = active_;
    active_ = true;
    return !was_active;
  }

  // Sets the task state to inactive (not on a run queue). Returns true if the
  // task was previously active.
  bool OnRemove() {
    const bool was_active = active_;
    active_ = false;
    return was_active;
  }

  // WAVLTree node state.
  fbl::WAVLTreeNodeState<Thread*> run_queue_node_;

  // The start time of the thread's current bandwidth request. This is the
  // virtual start time for fair tasks and the period start for deadline tasks.
  SchedTime start_time_{0};

  // The finish time of the thread's current bandwidth request. This is the
  // virtual finish time for fair tasks and the absolute deadline for deadline
  // tasks.
  SchedTime finish_time_{0};

  // Minimum finish time of all the descendants of this node in the run queue.
  // This value is automatically maintained by the WAVLTree observer hooks. The
  // value is used to perform a partition search in O(log n) time, to find the
  // thread with the earliest finish time that also has an eligible start time.
  SchedTime min_finish_time_{0};

  // Flag indicating whether this thread is associated with a run queue.
  bool active_{false};

  // The scheduling state of the thread.
  thread_state state_{THREAD_INITIAL};

  TA_GUARDED(thread_lock) BaseProfile base_profile_;
  TA_GUARDED(thread_lock) InheritedProfileValues inherited_profile_values_;
  TA_GUARDED(thread_lock) EffectiveProfile effective_profile_;

  // The current timeslice allocated to the thread.
  SchedDuration time_slice_ns_{0};

  // The runtime used in the current period.
  SchedDuration time_slice_used_ns_{0};

  // The total time in THREAD_RUNNING state. If the thread is currently in
  // THREAD_RUNNING state, this excludes the time accrued since it last left the
  // scheduler.
  SchedDuration runtime_ns_{0};

  // Tracks the exponential moving average of the runtime of the thread.
  SchedDuration expected_runtime_ns_{0};

  // Tracks runtime accumulated until voluntarily blocking or exhausting the
  // allocated time slice. Used to exclude involuntary preemption when updating
  // the expected runtime estimate to improve accuracy.
  SchedDuration banked_runtime_ns_{0};

  // The time the thread last ran. The exact point in time this value represents
  // depends on the thread state:
  //   * THREAD_RUNNING: The time of the last reschedule that selected the thread.
  //   * THREAD_READY: The time the thread entered the run queue.
  //   * Otherwise: The time the thread last ran.
  SchedTime last_started_running_{0};

  // Takes the value of Scheduler::generation_count_ + 1 at the time this node
  // is added to the run queue.
  uint64_t generation_{0};

  Thread* previous_thread_{nullptr};
  bool incoming_locked_{false};
  bool outgoing_locked_{false};

  // The current sched_latency flow id for this thread.
  uint64_t flow_id_{0};

  // The current CPU the thread is READY or RUNNING on, INVALID_CPU otherwise.
  cpu_num_t curr_cpu_{INVALID_CPU};

  // The last CPU the thread ran on. INVALID_CPU before it first runs.
  cpu_num_t last_cpu_{INVALID_CPU};

  // The next CPU the thread should run on after the thread's migrate function
  // is called.
  cpu_num_t next_cpu_{INVALID_CPU};

  // The set of CPUs the thread is permitted to run on. The thread is never
  // assigned to CPUs outside of this set.
  cpu_mask_t hard_affinity_{CPU_MASK_ALL};

  // The set of CPUs the thread should run on if possible. The thread may be
  // assigned to CPUs outside of this set if necessary.
  cpu_mask_t soft_affinity_{CPU_MASK_ALL};
};

FBL_ENABLE_ENUM_BITS(SchedulerState::ProfileDirtyFlag)

template <>
inline void SchedulerState::EffectiveProfileDirtyTracker<true>::MarkBaseProfileChanged() {
  dirty_flags_ |= ProfileDirtyFlag::BaseDirty;
}

template <>
inline void SchedulerState::EffectiveProfileDirtyTracker<true>::MarkInheritedProfileChanged() {
  dirty_flags_ |= ProfileDirtyFlag::InheritedDirty;
}

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_SCHEDULER_STATE_H_
