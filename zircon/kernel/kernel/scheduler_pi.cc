// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>

#include <kernel/owned_wait_queue.h>
#include <kernel/scheduler.h>
#include <kernel/scheduler_internal.h>
#include <kernel/scheduler_state.h>
#include <ktl/algorithm.h>
#include <ktl/type_traits.h>

// PiOperation is an inner class of Scheduler and the base class of each of the
// various PI operations we need to implement.  Its primary jobs are:
//
// 1) Provide accessors abstract the distinction between a thread and an owned
//    wait queue when working with templated methods who operate on an UpstreamType
//    and a TargetType, where each type might be either a Thread or an
//    OwnedWaitQueue.
// 2) Implement the common PI handler responsible for obtaining the proper
//    locks, and removing/re-inserting a target from/to its container while
//    updating the targets dynamic scheduling parameters.
// 3) Do all of this using CRTP instead of lambdas, allowing us to preserve
//    our static annotations all of the way through the operation instead of
//    needing to dynamically assert that we hold certain capabilities throughout
//    the operation.
template <typename Op, typename TargetType>
class Scheduler::PiOperation {
 protected:
  PiOperation(TargetType& target) TA_REQ(target.get_lock()) : target_(target) {}

  static void AssertEpDirtyState(const Thread& thread, SchedulerState::ProfileDirtyFlag expected)
      TA_REQ(thread.get_lock()) {
    thread.scheduler_state().effective_profile().AssertDirtyState(expected);
  }

  static SchedTime& GetStartTime(Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().start_time_;
  }

  static SchedTime& GetFinishTime(Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().finish_time_;
  }

  static SchedDuration& GetTimeSliceNs(Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().time_slice_ns_;
  }

  static SchedDuration& GetTimeSliceUsedNs(Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().time_slice_used_ns_;
  }

  static SchedTime GetStartTime(const Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().start_time_;
  }

  static SchedTime GetFinishTime(const Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().finish_time_;
  }

  static SchedDuration GetTimeSliceNs(const Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().time_slice_ns_;
  }

  static SchedDuration GetTimeSliceUsedNs(const Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().time_slice_used_ns_;
  }

  static SchedDuration GetRemainingTimeSliceNs(const Thread& thread) TA_REQ(thread.get_lock()) {
    return thread.scheduler_state().remaining_time_slice_ns();
  }

  // OwnedWaitQueues do not need to bother to track the dirty or clean state of
  // their implied effective profile.  They have no base profile (only inherited
  // values) which gets turned into an effective profile by the
  // EffectiveProfileHeper (see below) during a PI interaction.  We can get away
  // with this because OWQs:
  //
  // 1) Cannot exist in any collections where their position is determined by effective profile
  //    (otherwise we would need to remove and re-insert the node in the collection during an
  //    update).
  // 2) Cannot contribute to a scheduler's bookkeeping (because OWQs are not things which get
  //    scheduled).
  //
  static void AssertEpDirtyState(const OwnedWaitQueue& owq,
                                 SchedulerState::ProfileDirtyFlag expected) TA_REQ(owq.get_lock()) {
  }

  static SchedTime& GetStartTime(OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->start_time;
  }

  static SchedTime& GetFinishTime(OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->finish_time;
  }

  static SchedDuration& GetTimeSliceNs(OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->time_slice_ns;
  }

  static SchedDuration& GetTimeSliceUsedNs(OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->time_slice_used_ns;
  }

  static SchedTime GetStartTime(const OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->start_time;
  }

  static SchedTime GetFinishTime(const OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->finish_time;
  }

  static SchedDuration GetTimeSliceNs(const OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->time_slice_ns;
  }

  static SchedDuration GetTimeSliceUsedNs(const OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->time_slice_used_ns;
  }

  static SchedDuration GetRemainingTimeSliceNs(const OwnedWaitQueue& owq) TA_REQ(owq.get_lock()) {
    return owq.inherited_scheduler_state_storage()->time_slice_ns -
           owq.inherited_scheduler_state_storage()->time_slice_used_ns;
  }

  inline void HandlePiInteractionCommon() TA_REQ(chainlock_transaction_token, target_.get_lock());

  TargetType& target_;
};

namespace {
// Notes about ComputeEffectiveProfile and the EffectiveProfileHelper:
//
// Threads contain internal storage which holds their "effective profile", the
// combination of their base profile and all of their inherited profile
// pressure, as well as set of dirty/clean flags.
//
// Owned wait queues don't have quite the same arrangement.  They themselves
// have no base profile, and their effective profile is really only the their
// inherited deadline profile (if any), or the total of their inherited fair
// weight (if there is no inherited deadline).  They do not explicitly maintain
// storage for their effective profile.
//
// When we get to this point in profile propagation, however, we need to be able
// to compute 3 things:
//
// 1) The effective profile of the target node before recomputing it because of
//    the change in profile pressure.
// 2) The effective profile of the target node after recomputing it because of
//    the change in profile pressure (note that this is the same for OWQs, but
//    not threads)
// 3) The effective profile of the upstream node which gave rise to the change
//    in target profile pressure.
//
// For threads, we can just access the reference to the current effective
// profile when we need to know.  The non-templated Thread version of
// HandlePiCommon can latch the old ep into a local variable before recomputing,
// and pass the reference to both the old and new profiles to the injected
// callback.  Likewise, if a thread is the upstream node and an operation needs
// to know the effective profile of the upstream node, a reference to the
// thread's internal storage is all which is needed.
//
// OWQs are a bit more problematic as they don't have internal storage to
// reference.  We actually need to compute what the effective profile is based
// on the current IPVs, and store that result somewhere.  We would rather not
// perform this calculation when we don't have to, and we would also rather not
// copy a thread's effective profile into local stack allocated storage if we
// don't have to.
//
// This starts to become an issue in the operations themselves, whose node types
// are templated to keep the logic consistent even when the nodes involved are
// different combinations of Thread and OWQ.  In particular, when an operation
// captures the `upstream` member in a lambda callback, we cannot simply call
// `upstream.effective_profile()` to fetch a reference to internal storage (OWQs
// don't have any), nor do we want to copy the thread's internal storage to a
// local EP instance when we could have used a const reference to the thread's
// internal storage instead.
//
// The ComputeEffectiveProfile function and EffectiveProfileHelper class (below)
// help out with this situation.  ComputeEffectiveProfile defines the logic for
// determining what an OWQ's effective profile is based on its IPVs.  It is used
// in the OWQ specific version of HandlePiCommon to provide the old/new ep
// arguments to the callback.
//
// For local lambda captures, we use the EffectiveProfileHelper. The Thread
// specialized version just stores the pointer (something the compiler was going
// to cache in a register anyway), while the OWQ version actually allocates
// storage and uses ComputeEffectiveProfile to populate that storage when
// needed.
//
SchedulerState::EffectiveProfile ComputeEffectiveProfile(const OwnedWaitQueue& owq)
    TA_REQ(owq.get_lock()) {
  SchedulerState::EffectiveProfile ep{};

  DEBUG_ASSERT(owq.inherited_scheduler_state_storage() != nullptr);
  const SchedulerState::WaitQueueInheritedSchedulerState& iss =
      *owq.inherited_scheduler_state_storage();

  iss.ipvs.AssertConsistency();

  if (iss.ipvs.uncapped_utilization > SchedUtilization{0}) {
    DEBUG_ASSERT(iss.ipvs.min_deadline > SchedDuration{0});
    ep.SetDeadline({ktl::min(Scheduler::kThreadUtilizationMax, iss.ipvs.uncapped_utilization),
                    iss.ipvs.min_deadline});
  } else {
    // Note that we cannot assert that the total weight of this OWQ's IPVs has
    // dropped to zero at this point.  It is possible that there are threads
    // still in this queue, just none of them have inheritable profiles.
    ep.SetFair(iss.ipvs.total_weight);
  }

  return ep;
}

template <typename T>
class EffectiveProfileHelper;

template <>
class EffectiveProfileHelper<Thread> {
 public:
  EffectiveProfileHelper(const Thread& thread) TA_REQ(thread.get_lock())
      : effective_profile_ref_{thread.scheduler_state().effective_profile()} {}

  const SchedulerState::EffectiveProfile& operator()() const { return effective_profile_ref_; }

 private:
  const SchedulerState::EffectiveProfile& effective_profile_ref_;
};

template <>
class EffectiveProfileHelper<OwnedWaitQueue> {
 public:
  EffectiveProfileHelper(const OwnedWaitQueue& owq) TA_REQ(owq.get_lock())
      : effective_profile_{ComputeEffectiveProfile(owq)} {}

  const SchedulerState::EffectiveProfile& operator()() const { return effective_profile_; }

 private:
  const SchedulerState::EffectiveProfile effective_profile_;
};

template <typename T>
EffectiveProfileHelper(const T&) -> EffectiveProfileHelper<T>;

// Definition of the ThreadBaseProfileChanged operation.  Used to update a
// thread's effective profile and position in its container at the start of a
// base profile update operation, regardless of whether or not the target thread
// is currently blocked or currently assigned to a scheduler.
//
// Later on, if the thread happens to be an upstream member of a PI graph whose
// target is either an OwnedWaitQueue or another thread, the
// UpstreamThreadBaseProfileChanged operation will be triggered to handle the
// downstream target of the graph.
class ThreadBaseProfileChangedOp
    : public Scheduler::PiOperation<ThreadBaseProfileChangedOp, Thread> {
 public:
  using Base = Scheduler::PiOperation<ThreadBaseProfileChangedOp, Thread>;
  ThreadBaseProfileChangedOp(Thread& target) TA_REQ(target.get_lock())
      : Base{target}
#if !EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
        ,
        has_ever_run_{target.state() != thread_state::THREAD_INITIAL}
#endif
  {
  }

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
  void UpdateDynamicParams(const SchedulerState::EffectiveProfile& target_old_ep,
                           const SchedulerState::EffectiveProfile& target_new_ep,
                           SchedTime mono_now)
      TA_REQ(chainlock_transaction_token, target_.get_lock()) {
    // Make sure the start and finish times are consistent with the bandwidth
    // parameters of the deadline profile. Consistency in fair profiles is
    // handled by Scheduler::AdjustFairBandwidth.
    if (target_new_ep.IsDeadline()) {
      GetStartTime(target_) = GetFinishTime(target_) - target_new_ep.deadline().deadline_ns;
    }
  }
#else
  void UpdateDynamicParams(const SchedulerState::EffectiveProfile& target_old_ep,
                           const SchedulerState::EffectiveProfile& target_new_ep,
                           SchedTime mono_now, SchedTime virt_now)
      TA_REQ(chainlock_transaction_token, target_.get_lock()) {
    // When the base profile of a thread was changed by a user, we treat it like
    // a yield in order to avoid any attempts by a user to game the system to
    // get more bandwidth by constantly changing the base profile of their
    // thread(s).
    //
    // The exception to this is if the thread has been created, but has never
    // run before.  In this situation, we simply make the thread eligible to run
    // right now.
    if (!has_ever_run_) {
      GetStartTime(target_) = SchedTime{0};
      GetFinishTime(target_) = SchedTime{0};
    } else if (target_new_ep.IsFair()) {
      GetStartTime(target_) = virt_now;
      GetFinishTime(target_) = virt_now;
    } else {
      DEBUG_ASSERT(target_new_ep.IsDeadline());
      // TODO(johngro): use the `now` time latched at the start of ThreadBaseProfileChanged instead?
      GetStartTime(target_) = mono_now + target_new_ep.deadline().deadline_ns;
      GetFinishTime(target_) = GetStartTime(target_) + target_new_ep.deadline().deadline_ns;
    }
    GetTimeSliceNs(target_) = SchedDuration{0};
  }
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED

  void DoOperation() TA_REQ(chainlock_transaction_token) {
    // We held this lock at construction time and should never drop any locks
    // during our object's lifetime.  We should be able to simply Mark that we
    // hold it now instead of using a full Assert.
    target_.get_lock().MarkHeld();

    // The base profile of this thread has changed.  While there may or may not be
    // something downstream of this thread, we need to start by dealing with
    // updating this threads static and dynamic scheduling parameters first.
    AssertEpDirtyState(target_, SchedulerState::ProfileDirtyFlag::BaseDirty);
    HandlePiInteractionCommon();
  }

#if !EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
 private:
  const bool has_ever_run_;
#endif
};

// Definition of the UpstreamThreadBaseProfileChange operation.  Called when a
// thread in a graph whose target is either an OwnedWaitQueue or a different
// Thread changes its base profile in order update the target's new effective
// profile, position in container, and dynamic scheduling parameters.
//
template <typename TargetType>
class UpstreamThreadBaseProfileChangedOp
    : public Scheduler::PiOperation<UpstreamThreadBaseProfileChangedOp<TargetType>, TargetType> {
 public:
  UpstreamThreadBaseProfileChangedOp(const Thread& upstream, TargetType& target)
      TA_REQ(upstream.get_lock(), target.get_lock())
      : Base{target}, upstream_{upstream} {}

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
  void UpdateDynamicParams(const SchedulerState::EffectiveProfile& target_old_ep,
                           const SchedulerState::EffectiveProfile& target_new_ep,
                           SchedTime mono_now) TA_REQ(target_.get_lock()) {
    // Make sure the start and finish times are consistent with the bandwidth
    // parameters of the deadline profile. Consistency in fair profiles is
    // handled by Scheduler::AdjustFairBandwidth.
    if (target_new_ep.IsDeadline()) {
      GetStartTime(target_) = GetFinishTime(target_) - target_new_ep.deadline().deadline_ns;
    }
  }
#else
  void UpdateDynamicParams(const SchedulerState::EffectiveProfile& target_old_ep,
                           const SchedulerState::EffectiveProfile& target_new_ep,
                           SchedTime mono_now, SchedTime virt_now) TA_REQ(target_.get_lock()) {
    // TODO(johngro): What is the proper fair policy here?  Typically, we
    // penalize threads which are changing profiles to make sure there is no way
    // for them to game the system and gain any bandwidth via artificial
    // amplification.  We don't _really_ want to be punishing threads who are
    // having their parameters changed as a result of upstream base profile
    // changes, esp if folks start to allow cross process PI.
    //
    // For now, to keep things simple, we just penalize the target_ thread the
    // same way that we penalize any other thread.  Basically, don't write code
    // where you block a thread behind another thread and then start to change
    // its profile while blocked.
    if (target_new_ep.IsFair()) {
      GetStartTime(target_) = virt_now;
      GetFinishTime(target_) = virt_now;
      GetTimeSliceNs(target_) = SchedDuration{0};
    } else {
      DEBUG_ASSERT(target_new_ep.IsDeadline());
      GetStartTime(target_) = mono_now + target_new_ep.deadline().deadline_ns;
      GetFinishTime(target_) = GetStartTime(target_) + target_new_ep.deadline().deadline_ns;
      GetTimeSliceNs(target_) = SchedDuration{0};
    }
  }
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED

  void DoOperation() TA_REQ(chainlock_transaction_token) {
    // We held these locks at construction time and should never drop any locks
    // during our object's lifetime.  We should be able to simply Mark that we
    // hold them now instead of using a full Assert.
    upstream_.get_lock().MarkHeld();
    target_.get_lock().MarkHeld();

    // The base profile of a thread upstream of this target node has changed.  We need to
    // do the following:
    //
    // 1) Recompute the target's effective profile.
    // 2) Handle any bookkeeping updates for the scheduler's state, if the target
    //    is a thread which is either RUNNING or READY, and therefore has a
    //    scheduler assigned to it.
    // 3) Handle any updates to the target's dynamic scheduling parameters (eg,
    //    start time, finish time, time slice remaining)
    if constexpr (ktl::is_same_v<Thread, TargetType>) {
      DEBUG_ASSERT(&upstream_ != &target_);
    }

    AssertEpDirtyState(target_, SchedulerState::ProfileDirtyFlag::InheritedDirty);
    AssertEpDirtyState(upstream_, SchedulerState::ProfileDirtyFlag::Clean);
    Base::HandlePiInteractionCommon();
  }

 private:
  using Base = Scheduler::PiOperation<UpstreamThreadBaseProfileChangedOp<TargetType>, TargetType>;
  using Base::AssertEpDirtyState;
  using Base::GetFinishTime;
  using Base::GetRemainingTimeSliceNs;
  using Base::GetStartTime;
  using Base::GetTimeSliceNs;
  using Base::GetTimeSliceUsedNs;
  using Base::target_;
  const Thread& upstream_;
};

// Definition of the Join operation.  Called when a new edge is added connecting
// the target of one PI graph (the upstream node) to a different PI graph.
//
template <typename UpstreamType, typename TargetType>
class JoinNodeToPiGraphOp
    : public Scheduler::PiOperation<JoinNodeToPiGraphOp<UpstreamType, TargetType>, TargetType> {
 public:
  JoinNodeToPiGraphOp(const UpstreamType& upstream, TargetType& target)
      TA_REQ(upstream.get_lock(), target.get_lock())
      : Base{target}, upstream_{upstream} {}

  void UpdateDynamicParams(const SchedulerState::EffectiveProfile& target_old_ep,
                           const SchedulerState::EffectiveProfile& target_new_ep, SchedTime mono_now
#if !EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
                           ,
                           SchedTime virt_now
#endif
                           ) TA_REQ(chainlock_transaction_token, target_.get_lock()) {
    // See DoOperation for why it is ok to simply Mark instead of Assert here.
    upstream_.get_lock().MarkHeld();
    const EffectiveProfileHelper upstream_ep{upstream_};

    // If our upstream node is fair, then we have nothing more to do in the
    // common path.  Our target_'s effective profile has already been updated
    // appropriately, and no changes to the target_'s dynamic deadline scheduling
    // parameters needs to be done (since new pressure from a fair thread
    // currently has no effect on deadline utilization).  Any scheduler specific
    // side effects will be handled by the active thread path (below) if the
    // target_ is an active thread.
    if (upstream_ep().IsFair()) {
      return;
    }

    // Our upstream node is not a fair node, therefore it must be a deadline node.
    // In addition, no matter what it was before, our target_ node must now be a
    // deadline node.
    DEBUG_ASSERT(upstream_ep().IsDeadline());
    DEBUG_ASSERT(target_new_ep.IsDeadline());

    if (target_old_ep.IsFair()) {
      // If the target_ has just now become deadline, we can simply transfer the
      // dynamic deadline parameters from upstream to the target_.
      GetStartTime(target_) = GetStartTime(upstream_);
      GetFinishTime(target_) = GetFinishTime(upstream_);
      GetTimeSliceNs(target_) = GetTimeSliceNs(upstream_);
      GetTimeSliceUsedNs(target_) = GetTimeSliceUsedNs(upstream_);
    } else {
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      // The target_ was already a deadline thread, then we need to recompute the
      // target_'s dynamic deadline parameters using the lag equation.
      // Compute the remaining periods of the target and upstream threads.
      const SchedDuration target_remaining_period =
          ktl::max<SchedDuration>(GetFinishTime(target_) - mono_now, SchedDuration{0});
      const SchedDuration upstream_remaining_period =
          ktl::max<SchedDuration>(GetFinishTime(upstream_) - mono_now, SchedDuration{0});
      const SchedDuration min_remaining_period =
          ktl::min(target_remaining_period, upstream_remaining_period);

      GetFinishTime(target_) = ktl::min(GetFinishTime(target_), GetFinishTime(upstream_));
      GetStartTime(target_) = GetFinishTime(target_) - target_new_ep.deadline().deadline_ns;

      // TODO(eieio): If a period is expired, the full bandwidth contribution of
      // the respective task is available to the target and downstream, if any.
      const SchedDuration new_remaining_time_slice =
          GetRemainingTimeSliceNs(target_) + GetRemainingTimeSliceNs(upstream_) +
          target_old_ep.deadline().utilization * (min_remaining_period - target_remaining_period) +
          upstream_ep().deadline().utilization * (min_remaining_period - upstream_remaining_period);

      // Limit the TSR.  It cannot be less than zero nor can it be more than the
      // time until the absolute deadline of the new combined thread.
      //
      // TODO(johngro): If we did have to clamp the TSR, the amount we clamp by
      // needs to turn into carried lag.
      const SchedDuration clamped_remaining_time_slice = ktl::clamp<SchedDuration>(
          new_remaining_time_slice, SchedDuration{0}, min_remaining_period);
      GetTimeSliceNs(target_) = target_new_ep.deadline().capacity_ns;
      GetTimeSliceUsedNs(target_) =
          target_new_ep.deadline().capacity_ns - clamped_remaining_time_slice;
      DEBUG_ASSERT_MSG(GetRemainingTimeSliceNs(target_) >= 0,
                       "capacity=%" PRId64 " remaining_time_slice=%" PRId64
                       " remaining_period=%" PRId64,
                       target_new_ep.deadline().capacity_ns.raw_value(),
                       new_remaining_time_slice.raw_value(), min_remaining_period.raw_value());
#else
      // The target_ was already a deadline thread, then we need to recompute the
      // target_'s dynamic deadline parameters using the lag equation.
      // Compute the time till absolute deadline (ttad) of the target_ and
      // upstream threads.
      const SchedDuration target_ttad = (GetFinishTime(target_) > mono_now)
                                            ? (GetFinishTime(target_) - mono_now)
                                            : SchedDuration{0};
      const SchedDuration upstream_ttad = (GetFinishTime(upstream_) > mono_now)
                                              ? (GetFinishTime(upstream_) - mono_now)
                                              : SchedDuration{0};
      const SchedDuration combined_ttad = ktl::min(target_ttad, upstream_ttad);

      GetFinishTime(target_) = ktl::min(GetFinishTime(target_), GetFinishTime(upstream_));
      GetStartTime(target_) = GetFinishTime(target_) - target_new_ep.deadline().deadline_ns;

      const SchedDuration new_tsr = GetTimeSliceNs(target_) + GetTimeSliceNs(upstream_) +
                                    (target_new_ep.deadline().utilization * combined_ttad) -
                                    (target_old_ep.deadline().utilization * target_ttad) -
                                    (upstream_ep().deadline().utilization * upstream_ttad);

      // Limit the TSR.  It cannot be less than zero nor can it be more than the
      // time until the absolute deadline of the new combined thread.
      //
      // TODO(johngro): If we did have to clamp the TSR, the amount we clamp by
      // needs to turn into carried lag.
      GetTimeSliceNs(target_) = ktl::clamp<SchedDuration>(new_tsr, SchedDuration{0}, combined_ttad);
      GetFinishTime(target_) = ktl::min(GetFinishTime(target_), GetFinishTime(upstream_));
      GetStartTime(target_) = GetFinishTime(target_) - target_new_ep.deadline().deadline_ns;
#endif
    }
  }

  void DoOperation() TA_REQ(chainlock_transaction_token) {
    // We held these locks at construction time and should never drop any locks
    // during our object's lifetime.  We should be able to simply Mark that we
    // hold them now instead of using a full Assert.
    upstream_.get_lock().MarkHeld();
    target_.get_lock().MarkHeld();

    if constexpr (ktl::is_same_v<UpstreamType, TargetType>) {
      DEBUG_ASSERT(&upstream_ != &target_);
    }

    AssertEpDirtyState(target_, SchedulerState::ProfileDirtyFlag::InheritedDirty);
    AssertEpDirtyState(upstream_, SchedulerState::ProfileDirtyFlag::Clean);
    Base::HandlePiInteractionCommon();
  }

 private:
  using Base = Scheduler::PiOperation<JoinNodeToPiGraphOp<UpstreamType, TargetType>, TargetType>;
  using Base::AssertEpDirtyState;
  using Base::GetFinishTime;
  using Base::GetRemainingTimeSliceNs;
  using Base::GetStartTime;
  using Base::GetTimeSliceNs;
  using Base::GetTimeSliceUsedNs;
  using Base::target_;

  const UpstreamType& upstream_;
};

// Definition of the Split operation.  Called when an upstream node has its
// downstream edge removed, splitting it from the PI graph it was a member of
// and becoming the target of a new graph in the process.
//
template <typename UpstreamType, typename TargetType>
class SplitNodeFromPiGraphOp
    : public Scheduler::PiOperation<SplitNodeFromPiGraphOp<UpstreamType, TargetType>, TargetType> {
 public:
  SplitNodeFromPiGraphOp(const UpstreamType& upstream, TargetType& target)
      TA_REQ(upstream.get_lock(), target.get_lock())
      : Base{target}, upstream_{upstream} {}

  void UpdateDynamicParams(const SchedulerState::EffectiveProfile& target_old_ep,
                           const SchedulerState::EffectiveProfile& target_new_ep, SchedTime mono_now
#if !EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
                           ,
                           SchedTime virt_now
#endif
                           ) TA_REQ(chainlock_transaction_token, target_.get_lock()) {
    // During construction, we statically required that we were holding
    // upstream_.get_lock().  It should be OK to Mark it as held here.
    upstream_.get_lock().MarkHeld();
    const EffectiveProfileHelper upstream_ep{upstream_};

    // Was the target_ node a fair node?  If so, there is really nothing for us
    // to do here.
    if (target_old_ep.IsFair()) {
      return;
    }

    DEBUG_ASSERT(target_old_ep.IsDeadline());
    if (target_new_ep.IsFair()) {
      // If target_ node is now a fair node, then the upstream_ node must have been
      // a deadline node.  This split operation is what caused the target_ node to
      // change from deadline to fair, all of the deadline pressure must have been
      // coming from the upstream_ node.  Assert all of this.
      DEBUG_ASSERT(upstream_ep().IsDeadline());
      DEBUG_ASSERT_MSG(target_old_ep.deadline().capacity_ns == upstream_ep().deadline().capacity_ns,
                       "toep.deadline.capacity=%" PRId64 " uep.deadline.capacity=%" PRId64,
                       target_old_ep.deadline().capacity_ns.raw_value(),
                       upstream_ep().deadline().capacity_ns.raw_value());
      DEBUG_ASSERT_MSG(target_old_ep.deadline().deadline_ns == upstream_ep().deadline().deadline_ns,
                       "toep.deadline.deadline=%" PRId64 " uep.deadline.deadline=%" PRId64,
                       target_old_ep.deadline().deadline_ns.raw_value(),
                       upstream_ep().deadline().deadline_ns.raw_value());

      // Give the dynamic deadline parameters over to the upstream_ node.
      GetStartTime(upstream_) = GetStartTime(target_);
      GetFinishTime(upstream_) = GetFinishTime(target_);
      GetTimeSliceNs(upstream_) = GetTimeSliceNs(target_);
      GetTimeSliceUsedNs(upstream_) = GetTimeSliceUsedNs(target_);

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      // TODO(eieio): Just expire the time slice for now. This should actually
      // be split out the same way as for deadline threads.
      GetTimeSliceUsedNs(target_) = GetTimeSliceNs(target_);
#else
      // Make sure that our fair parameters have been reset.  If we are
      // an active thread, we will now re-arrive with our new parameters.
      GetStartTime(target_) = SchedTime{0};
      GetFinishTime(target_) = SchedTime{1};
      GetTimeSliceNs(target_) = SchedDuration{0};
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
    } else {
      // OK, the target_ node is still a deadline node.  If the upstream_ node
      // is a fair node, we don't have to do anything at all.  A fair node
      // splitting off from a deadline node should not change the deadline
      // node's dynamic parameters.  If the upstream_ fair node is a thread, it is
      // going to arrive in a new scheduler queue Real Soon Now, and have new
      // dynamic parameters computed for it.
      //
      // If _both_ nodes are deadline nodes, then we need to invoke the lag
      // equation in order to figure out what the new time slice remaining and
      // absolute deadlines are.
      if (upstream_ep().IsDeadline()) {
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
        // Compute the time until absolute deadline of the target and upstream.
        const SchedDuration target_remaining_period =
            ktl::max<SchedDuration>(GetFinishTime(target_) - mono_now, SchedDuration{0});
        const SchedDuration upstream_remaining_period =
            ktl::max<SchedDuration>(GetFinishTime(upstream_) - mono_now, SchedDuration{0});
#else
        // Compute the time till absolute deadline (ttad) of the target_.
        const SchedDuration target_ttad = (GetFinishTime(target_) > mono_now)
                                              ? (GetFinishTime(target_) - mono_now)
                                              : SchedDuration{0};
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED

        // Figure out what the uncapped utilization of the combined thread
        // _would_ have been based on the utilizations of the target_ and
        // upstream_ nodes after the split.  It is important when scaling
        // timeslices to be sure that we divide by a utilization value which
        // is the sum of the two (now separated) utilization values.
        const SchedUtilization combined_uncapped_utilization =
            target_new_ep.deadline().utilization + upstream_ep().deadline().utilization;

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
        const SchedUtilization upstream_utilization_ratio =
            upstream_ep().deadline().utilization / combined_uncapped_utilization;
        const SchedDuration new_upstream_remaining_time_slice =
            upstream_utilization_ratio * GetRemainingTimeSliceNs(target_) +
            upstream_ep().deadline().utilization *
                (upstream_remaining_period - target_remaining_period);

        // TODO(johngro): This also changes when carried lag comes into
        // play.
        GetTimeSliceNs(upstream_) = upstream_ep().deadline().capacity_ns;
        GetTimeSliceUsedNs(upstream_) =
            upstream_ep().deadline().capacity_ns -
            ktl::max(new_upstream_remaining_time_slice, SchedDuration{0});
#else
        // If the upstream_ node's time till absolute deadline is zero, there is
        // no need to compute its time slice remaining right now; we would just
        // end up capping it to zero anyway.
        //
        // TODO(johngro): this changes when carried lag comes into the picture.
        if (GetFinishTime(upstream_) <= mono_now) {
          GetTimeSliceNs(upstream_) = SchedDuration{0};
        } else {
          // Looks like we need to compute this value after all.
          const SchedDuration upstream_ttad = GetFinishTime(upstream_) - mono_now;
          const SchedDuration new_upstream_tsr =
              upstream_ep().deadline().utilization *
              ((GetTimeSliceNs(target_) / combined_uncapped_utilization) + upstream_ttad -
               target_ttad);

          // TODO(johngro): This also changes when carried lag comes into
          // play.
          GetTimeSliceNs(upstream_) = ktl::max(new_upstream_tsr, SchedDuration{0});
        }
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED

        // TODO(johngro): Fix this.  Logically, it is not correct to
        // preserve the abs deadline of the target_ after the split.  The
        // target_'s bookkeeping should be equivalent to the values which
        // would be obtained by joining all of the threads which exist
        // upstream_ of this node together.  Because of this, our new target_
        // finish time should be equal to the min across all finish times
        // immediately upstream_ of this node.
        //
        // Now handle the target_ node.  We preserve the absolute deadline of
        // the target_ node before and after the split, so we need to
        // recompute its start time so that the distance between the
        // absolute deadline and the start time is equal to the new relative
        // deadline of the target_.
        GetStartTime(target_) = GetFinishTime(target_) - target_new_ep.deadline().deadline_ns;

        // The time till absolute deadline of the pre and post split target_
        // remains the same, so the ttad contributions to the timeslice
        // remaining simply drop out of the lag equation.
        //
        // Note that fixed point division takes the precision of the
        // assignee into account to provide headroom in certain situations.
        // Use an intermediate with the same fractional precision as the
        // utilization operands before scaling the non-fractional timeslice.
        const SchedUtilization target_utilization_ratio =
            target_new_ep.deadline().utilization / combined_uncapped_utilization;
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
        const SchedDuration new_target_remaining_time_slice =
            GetRemainingTimeSliceNs(target_) * target_utilization_ratio;

        GetTimeSliceNs(target_) = target_new_ep.deadline().capacity_ns;
        GetTimeSliceUsedNs(target_) = target_new_ep.deadline().capacity_ns -
                                      ktl::max(new_target_remaining_time_slice, SchedDuration{0});
#else
        const SchedDuration new_target_tsr = GetTimeSliceNs(target_) * target_utilization_ratio;

        // TODO(johngro): once again, need to consider carried lag here.
        GetTimeSliceNs(target_) = ktl::max(new_target_tsr, SchedDuration{0});
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      }
    }
  }

  void DoOperation() TA_REQ(chainlock_transaction_token) {
    // We held these locks at construction time and should never drop any locks
    // during our object's lifetime.  We should be able to simply Mark that we
    // hold them now instead of using a full Assert.
    upstream_.get_lock().MarkHeld();
    target_.get_lock().MarkHeld();

    if constexpr (ktl::is_same_v<UpstreamType, TargetType>) {
      DEBUG_ASSERT(&upstream_ != &target_);
    }

    AssertEpDirtyState(target_, SchedulerState::ProfileDirtyFlag::InheritedDirty);
    AssertEpDirtyState(upstream_, SchedulerState::ProfileDirtyFlag::Clean);
    Base::HandlePiInteractionCommon();
  }

 private:
  using Base = Scheduler::PiOperation<SplitNodeFromPiGraphOp<UpstreamType, TargetType>, TargetType>;
  using Base::AssertEpDirtyState;
  using Base::GetFinishTime;
  using Base::GetRemainingTimeSliceNs;
  using Base::GetStartTime;
  using Base::GetTimeSliceNs;
  using Base::GetTimeSliceUsedNs;
  using Base::target_;

  const UpstreamType& upstream_;
};

}  // namespace

// Handle all of the common tasks associated with each of the possible PI
// interactions.  The outline of this is:
//
// 1) If the target is an active thread (meaning either running or runnable),
//    we need to:
// 1.1) Enter the scheduler's queue lock.
// 1.2) If the thread is active, but not actually running, remove the target
//      thread from its scheduler's run queue if it is in the queue.
// 1.3) Now update the thread's effective profile.
// 1.4) Apply any changes in the thread's effective profile to its scheduler's
//      bookkeeping.
// 1.5) Update the dynamic parameters of the thread.
// 1.6) Either re-insert the thread into its scheduler's run queue (if it was
//      READY AND in the queue) or adjust its schedulers preemption time (if it
//      was RUNNING).
// 1.7) Trigger a reschedule of the the thread's CPU.
// 2) If the target is either an OwnedWaitQueue, or a thread which is not
//    active:
// 2.1) Recompute the target's effective profile, adjust the target's position
//      in it's wait queue if the target is a thread which is currently
//      blocked in a wait queue.
// 2.2) Recompute the target's dynamic scheduler parameters.
template <typename Op, typename TargetType>
inline void Scheduler::PiOperation<Op, TargetType>::HandlePiInteractionCommon() {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "HandlePiInteractionCommon");

  if constexpr (ktl::is_same_v<TargetType, Thread>) {
    SchedulerState& state = target_.scheduler_state();

    if (const cpu_num_t curr_cpu = state.curr_cpu_; curr_cpu != INVALID_CPU) {
      DEBUG_ASSERT_MSG((target_.state() == THREAD_RUNNING) || (target_.state() == THREAD_READY),
                       "Unexpected target_ state %u for tid %" PRIu64 "\n", target_.state(),
                       target_.tid());

      Scheduler& scheduler = *Get(curr_cpu);
      Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler.queue_lock_, SOURCE_TAG};
      scheduler.ValidateInvariants();
      scheduler.AssertInScheduler(target_);

      // Sample the current time after acquiring the queue lock to avoid large skews under
      // contention.
      const SchedMonoTimeAndBootTicks now = CurrentMonoTimeAndBootTicks();

      // Keep track of the original disposition. See SchedulerQueueState for
      // more details on how the disposition and thread state indicate which
      // operations are permitted on the thread and its associated scheduler
      // bookkeeping.
      const Disposition disposition = target_.disposition();
      DEBUG_ASSERT_MSG(disposition != Disposition::Unassociated,
                       "Found unassociated thread %s with tid %lu in state %d, curr_cpu is: %u\n",
                       target_.name(), target_.tid(), target_.state(),
                       target_.scheduler_state().curr_cpu());

      if (target_.state() == THREAD_READY) {
        if (disposition == Disposition::Enqueued) {
          scheduler.EraseFromQueue(&target_);
        } else {
          CountUpdateInTransition();
        }
      } else {
        DEBUG_ASSERT(disposition == Disposition::Associated);
        ktrace::Scope trace_update = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "Update Timeslice",
                                                              ("target cpu", scheduler.this_cpu()));

        // Update the time slice before updating other bookkeeping.
        const SchedDuration actual_runtime_ns =
            ktl::max<SchedDuration>(now.mono_time - state.last_started_running_, SchedDuration{0});
        const SchedDuration scaled_actual_runtime_ns = state.effective_profile().IsDeadline()
                                                           ? scheduler.ScaleDown(actual_runtime_ns)
                                                           : actual_runtime_ns;

        state.runtime_ns_ += actual_runtime_ns;
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
        state.time_slice_used_ns_ += scaled_actual_runtime_ns;
#else
        const SchedDuration new_tsr = (state.time_slice_ns_ <= scaled_actual_runtime_ns)
                                          ? SchedDuration{0}
                                          : (state.time_slice_ns_ - scaled_actual_runtime_ns);
        state.time_slice_ns_ = new_tsr;
        if (EffectiveProfile& cur_ep = state.effective_profile_; cur_ep.IsFair()) {
          cur_ep.set_normalized_timeslice_remainder(
              new_tsr / ktl::max(cur_ep.initial_time_slice_ns(), SchedDuration{1}));
        };
#endif

        state.last_started_running_ = now.mono_time;
        scheduler.start_of_current_time_slice_ns_ = now.mono_time;
        scheduler.UpdateEstimatedEnergyConsumption(&target_, now, actual_runtime_ns);

        trace_update = KTRACE_END_SCOPE(("mono_now", now.mono_time), ("boot_ticks", now.boot_ticks),
                                        ("actual_runtime_ns", actual_runtime_ns));
      }

      // Copy the original effective profile before updating it to compute the
      // changes to the scheduler bookkeeping.
      const EffectiveProfile old_ep = state.effective_profile();
      target_.RecomputeEffectiveProfile();
      const EffectiveProfile& new_ep = state.effective_profile();

      // Update the scheduler bookkeeping, if necessary.
      if (disposition == Disposition::Associated || disposition == Disposition::Enqueued) {
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
        SchedWeight weight_delta{0};
        SchedUtilization utilization_delta{0};

        if (old_ep.IsFair()) {
          weight_delta -= old_ep.weight();
        } else {
          utilization_delta -= old_ep.deadline().utilization;
        }
        if (new_ep.IsFair()) {
          weight_delta += new_ep.weight();
        } else {
          utilization_delta += new_ep.deadline().utilization;
        }

        if (weight_delta != 0) {
          scheduler.weight_total_ += weight_delta;
          scheduler.UpdateFairBandwidthPeriod(now.mono_time);
        }
        if (utilization_delta != 0) {
          scheduler.UpdateTotalDeadlineUtilization(utilization_delta);
        }
#else
        if (old_ep.IsFair()) {
          scheduler.weight_total_ -= old_ep.weight();
          --scheduler.runnable_fair_task_count_;
        } else {
          scheduler.UpdateTotalDeadlineUtilization(-old_ep.deadline().utilization);
          --scheduler.runnable_deadline_task_count_;
        }

        if (new_ep.IsFair()) {
          scheduler.weight_total_ += new_ep.weight();
          ++scheduler.runnable_fair_task_count_;
        } else {
          scheduler.UpdateTotalDeadlineUtilization(new_ep.deadline().utilization);
          ++scheduler.runnable_deadline_task_count_;
        }
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      }

      DEBUG_ASSERT(scheduler.weight_total_ >= SchedWeight{0});
      DEBUG_ASSERT(scheduler.power_level_control_.normalized_utilization() >= SchedUtilization{0});

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      static_cast<Op*>(this)->UpdateDynamicParams(old_ep, new_ep, now.mono_time);
#else
      static_cast<Op*>(this)->UpdateDynamicParams(old_ep, new_ep, now.mono_time,
                                                  scheduler.virtual_time_);
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED

      if (target_.state() == THREAD_READY) {
        if (disposition == Disposition::Enqueued) {
          scheduler.QueueThread(&target_, Placement::Adjustment);
        }
      } else {
        DEBUG_ASSERT(target_.state() == THREAD_RUNNING);
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
        if (new_ep.IsFair()) {
          scheduler.target_preemption_time_ns_ =
              scheduler.start_of_current_time_slice_ns_ + state.remaining_time_slice_ns();
        } else {
          const SchedDuration scaled_remaining_time_slice_ns =
              scheduler.ScaleUp(state.remaining_time_slice_ns());
          scheduler.target_preemption_time_ns_ = ktl::min<SchedTime>(
              scheduler.start_of_current_time_slice_ns_ + scaled_remaining_time_slice_ns,
              state.finish_time_);
        }
#else
        scheduler.target_preemption_time_ns_ =
            scheduler.start_of_current_time_slice_ns_ + scheduler.ScaleUp(state.time_slice_ns_);
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      }

      // Check that target is left in the same disposition.
      DEBUG_ASSERT(disposition == target_.disposition());

      // We have made a change to this scheduler's state, we need to trigger a
      // reschedule operation as soon as we can.
      RescheduleMask(cpu_num_to_mask(state.curr_cpu_));
      scheduler.ValidateInvariants();
    } else {
      // We are dealing with a target_ which is a non-active thread (it has no
      // scheduler assigned). If the thread is blocked in a wait queue, update
      // its position in the wait queue while also updating its effective
      // profile.  Otherwise, simply update its effective profile.  Once that is
      // all done, update the dynamic parameters of the target_ using the
      // callback provided by the specific operation.
      SchedulerState::EffectiveProfile old_ep = state.effective_profile_;
      if (WaitQueue* wq = target_.wait_queue_state().blocking_wait_queue_; wq != nullptr) {
        // Note that to update our position in the WaitQueue this thread is
        // blocked in, we need to holding that wait queue's lock.  (It has to be
        // a WaitQueue and not an OwnedWaitQueue, or the PI operation's target_
        // would be the final OWQ, not the blocked Thread.)
        //
        // This should always be the case.  We need to holding the entire PI
        // chain during PI propagation.  That said, this is pretty much an
        // impossible thing to represent using static annotations, so we need to
        // fall back on a dynamic assert here instead.  We know that we are
        // holding the thread locked (because of all of the static annotations),
        // so we can use the token that is currently locking the thread's lock
        // to verify that the WaitQueue is both locked, and part of the same
        // locked chain that this operation owns and is currently locking the
        // thread.
        wq->get_lock().AssertHeld();
        wq->UpdateBlockedThreadEffectiveProfile(target_);
      } else {
        target_.RecomputeEffectiveProfile();
      }
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      static_cast<Op*>(this)->UpdateDynamicParams(old_ep, state.effective_profile_, CurrentTime());
#else
      static_cast<Op*>(this)->UpdateDynamicParams(old_ep, state.effective_profile_, CurrentTime(),
                                                  SchedTime{0});
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
    }
  } else {
    static_assert(ktl::is_same_v<TargetType, OwnedWaitQueue>,
                  "Targets of PI operations must either be Threads or OwnedWaitQueues");
    SchedulerState::EffectiveProfile old_ep{ComputeEffectiveProfile(target_)};
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
    static_cast<Op*>(this)->UpdateDynamicParams(old_ep, old_ep, CurrentTime());
#else
    static_cast<Op*>(this)->UpdateDynamicParams(old_ep, old_ep, CurrentTime(), SchedTime{0});
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
  }

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
  // The finish time should not be negative, but the start time can be when it
  // is re-calculated relative to the finish time.
#else
  DEBUG_ASSERT_MSG(SchedTime start_time = GetStartTime(target_);
                   start_time >= 0, "start_time %ld\n", start_time.raw_value());
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
  DEBUG_ASSERT_MSG(SchedTime finish_time = GetFinishTime(target_);
                   finish_time >= 0, "finish_time %ld\n", finish_time.raw_value());
}

void Scheduler::ThreadBaseProfileChanged(Thread& thread) {
  ThreadBaseProfileChangedOp op{thread};
  op.DoOperation();
}

template <typename TargetType>
void Scheduler::UpstreamThreadBaseProfileChanged(const Thread& upstream, TargetType& target) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_pi: base profile changed");
  UpstreamThreadBaseProfileChangedOp op{upstream, target};
  op.DoOperation();
}

template <typename UpstreamType, typename TargetType>
void Scheduler::JoinNodeToPiGraph(const UpstreamType& upstream, TargetType& target) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_pi: join");
  JoinNodeToPiGraphOp op{upstream, target};
  op.DoOperation();
}

template <typename UpstreamType, typename TargetType>
void Scheduler::SplitNodeFromPiGraph(const UpstreamType& upstream, TargetType& target) {
  ktrace::Scope trace = LOCAL_KTRACE_BEGIN_SCOPE(COMMON, "sched_pi: split");
  SplitNodeFromPiGraphOp op{upstream, target};
  op.DoOperation();
}

template void Scheduler::UpstreamThreadBaseProfileChanged(const Thread&, Thread&);
template void Scheduler::UpstreamThreadBaseProfileChanged(const Thread&, OwnedWaitQueue&);

template void Scheduler::JoinNodeToPiGraph(const Thread&, Thread&);
template void Scheduler::JoinNodeToPiGraph(const Thread&, OwnedWaitQueue&);
template void Scheduler::JoinNodeToPiGraph(const OwnedWaitQueue&, Thread&);
template void Scheduler::JoinNodeToPiGraph(const OwnedWaitQueue&, OwnedWaitQueue&);

template void Scheduler::SplitNodeFromPiGraph(const Thread&, Thread&);
template void Scheduler::SplitNodeFromPiGraph(const Thread&, OwnedWaitQueue&);
template void Scheduler::SplitNodeFromPiGraph(const OwnedWaitQueue&, Thread&);
template void Scheduler::SplitNodeFromPiGraph(const OwnedWaitQueue&, OwnedWaitQueue&);
