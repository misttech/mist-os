// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>

#include <kernel/owned_wait_queue.h>
#include <kernel/scheduler_internal.h>
#include <ktl/algorithm.h>
#include <ktl/type_traits.h>

#include "kernel/scheduler.h"

// Profile inheritance graphs (like all graphs) are directed graphs made up of a
// set of nodes which express the relationship between various threads and the
// wait queues they are waiting in.  Every node in the graph is either a Thread,
// or a WaitQueue.  Nodes in the graph always alternate between threads (blocked
// in a wait queue) and wait queues (owned by a thread).  While these graphs can
// be arbitrarily deep and arbitrarily wide, they are always convergent, meaning
// that they always terminate at a single node (called the "target").
//
// When propagating the consequences of a mutation to a PI graph, it is possible
// for the target node to be either a Thread or a WaitQueue.  The PiNodeAdapter
// class is a templated class meant to be fully specialized in order to allow
// the PI bookkeeping code to access the various important pieces of bookkeeping
// in the target node regardless of whether or not the target node is a thread
// or a wait queue instance.
//
template <>
class Scheduler::PiNodeAdapter<Thread> {
 public:
  explicit PiNodeAdapter(Thread& thread) : thread_(thread) {}

  // No copy, no move.
  PiNodeAdapter(const PiNodeAdapter&) = delete;
  PiNodeAdapter(PiNodeAdapter&&) = delete;
  PiNodeAdapter& operator=(const PiNodeAdapter&) = delete;
  PiNodeAdapter& operator=(PiNodeAdapter&&) = delete;

  Thread& thread() { return thread_; }

  const SchedulerState::EffectiveProfile& effective_profile() {
    return thread_.scheduler_state().effective_profile_;
  }

  void RecomputeEffectiveProfile() TA_REQ(thread_lock) {
    thread_.scheduler_state().RecomputeEffectiveProfile();
  }

  void AssertEpDirtyState(SchedulerState::ProfileDirtyFlag expected) TA_REQ(thread_lock) {
    thread_.scheduler_state().effective_profile_.AssertDirtyState(expected);
  }

  SchedTime& start_time() { return thread_.scheduler_state().start_time_; }
  SchedTime& finish_time() { return thread_.scheduler_state().finish_time_; }
  SchedDuration& time_slice_ns() { return thread_.scheduler_state().time_slice_ns_; }

 private:
  Thread& thread_;
};

template <>
class Scheduler::PiNodeAdapter<OwnedWaitQueue> {
 public:
  explicit PiNodeAdapter(OwnedWaitQueue& owq) : owq_(owq) {
    DEBUG_ASSERT(owq.inherited_scheduler_state_storage() != nullptr);
    SchedulerState::WaitQueueInheritedSchedulerState& iss =
        *owq.inherited_scheduler_state_storage();
    if (iss.ipvs.uncapped_utilization > SchedUtilization{0}) {
      DEBUG_ASSERT(iss.ipvs.min_deadline > SchedDuration{0});
      effective_profile_.discipline = SchedDiscipline::Deadline;
      effective_profile_.deadline = SchedDeadlineParams{
          ktl::min(Scheduler::kThreadUtilizationMax, iss.ipvs.uncapped_utilization),
          iss.ipvs.min_deadline};

    } else {
      // Note that we cannot assert that the total weight of this OWQ's IPVs has
      // dropped to zero at this point.  It is possible that there are threads
      // still in this queue, just none of them have inheritable profiles.
      effective_profile_.discipline = SchedDiscipline::Fair;
      effective_profile_.fair.weight = iss.ipvs.total_weight;
    }
  }

  // No copy, no move.
  PiNodeAdapter(const PiNodeAdapter&) = delete;
  PiNodeAdapter(PiNodeAdapter&&) = delete;
  PiNodeAdapter& operator=(const PiNodeAdapter&) = delete;
  PiNodeAdapter& operator=(PiNodeAdapter&&) = delete;

  OwnedWaitQueue& owq() { return owq_; }

  const SchedulerState::EffectiveProfile& effective_profile() { return effective_profile_; }
  void RecomputeEffectiveProfile() TA_REQ(thread_lock) {}

  // OwnedWaitQueues do not need to bother to track the dirty or clean state of their implied
  // effective profile.  They have no base profile (only inherited values) which gets turned into an
  // effective profile by the PiNodeAdapter during a PI interaction.  We can get away with this
  // because OWQs:
  //
  // 1) Cannot exist in any collections where their position is determined by effective profile
  //    (otherwise we would need to remove and re-insert the node in the collection during an
  //    update).
  // 2) Cannot contribute to a scheduler's bookkeeping (because OWQs are not things which get
  //    scheduled).
  //
  void AssertEpDirtyState(SchedulerState::ProfileDirtyFlag expected) TA_REQ(thread_lock) {}

  SchedTime& start_time() { return owq_.inherited_scheduler_state_storage()->start_time; }
  SchedTime& finish_time() { return owq_.inherited_scheduler_state_storage()->finish_time; }
  SchedDuration& time_slice_ns() { return owq_.inherited_scheduler_state_storage()->time_slice_ns; }

 private:
  OwnedWaitQueue& owq_;
  SchedulerState::EffectiveProfile effective_profile_{};
};

template <typename TargetType, typename Callable>
inline void Scheduler::HandlePiInteractionCommon(SchedTime now, PiNodeAdapter<TargetType>& target,
                                                 Callable UpdateDynamicParams) {
  if constexpr (ktl::is_same_v<TargetType, Thread>) {
    Thread& thread = target.thread();
    SchedulerState& ss = thread.scheduler_state();

    if (const cpu_num_t curr_cpu = ss.curr_cpu_; curr_cpu != INVALID_CPU) {
      DEBUG_ASSERT_MSG((thread.state() == THREAD_RUNNING) || (thread.state() == THREAD_READY),
                       "Unexpected thread state %u for tid %" PRIu64 "\n", thread.state(),
                       thread.tid());

      Scheduler& scheduler = *Get(curr_cpu);
      Guard<MonitoredSpinLock, NoIrqSave> queue_guard{&scheduler.queue_lock_, SOURCE_TAG};
      scheduler.ValidateInvariants();
      SchedulerState& tss = thread.scheduler_state();

      if (thread.state() == THREAD_READY) {
        // If the thread is in the READY state, we need to remove it from its
        // scheduler's run queue before updating the effective profile.  We'll
        // put it back in when we are done with the update.
        DEBUG_ASSERT(tss.active());
        scheduler.EraseFromQueue(&thread);
      } else {
        // The target thread's state is RUNNING.  Make sure to update its TSR
        // before we update either the dynamic parameters, or the scheduler's
        // parameters which depend on our static profile parameters.
        const SchedDuration actual_runtime_ns = now - tss.last_started_running_;
        const SchedDuration scaled_actual_runtime_ns = tss.effective_profile().IsDeadline()
                                                           ? scheduler.ScaleDown(actual_runtime_ns)
                                                           : actual_runtime_ns;

        tss.runtime_ns_ += actual_runtime_ns;
        thread.UpdateSchedulerStats({.state = thread.state(),
                                     .state_time = now.raw_value(),
                                     .cpu_time = actual_runtime_ns.raw_value()});
        const SchedDuration new_tsr = (tss.time_slice_ns_ <= scaled_actual_runtime_ns)
                                          ? SchedDuration{0}
                                          : (tss.time_slice_ns_ - scaled_actual_runtime_ns);
        tss.time_slice_ns_ = new_tsr;
        if (EffectiveProfile& cur_ep = tss.effective_profile_; cur_ep.IsFair()) {
          cur_ep.fair.normalized_timeslice_remainder =
              new_tsr / ktl::max(cur_ep.fair.initial_time_slice_ns, SchedDuration{1});
        };

        tss.last_started_running_ = now;
        scheduler.start_of_current_time_slice_ns_ = now;
      }

      // Go ahead and update the effective profile.
      const EffectiveProfile old_ep = ss.effective_profile();
      target.RecomputeEffectiveProfile();
      const EffectiveProfile& new_ep = ss.effective_profile();

      // Deal with the scheduler's bookkeeping.
      if (old_ep.IsFair()) {
        scheduler.weight_total_ -= old_ep.fair.weight;
        --scheduler.runnable_fair_task_count_;
      } else {
        scheduler.UpdateTotalDeadlineUtilization(-old_ep.deadline.utilization);
        --scheduler.runnable_deadline_task_count_;
      }

      if (new_ep.IsFair()) {
        scheduler.weight_total_ += new_ep.fair.weight;
        ++scheduler.runnable_fair_task_count_;
      } else {
        scheduler.UpdateTotalDeadlineUtilization(new_ep.deadline.utilization);
        ++scheduler.runnable_deadline_task_count_;
      }

      DEBUG_ASSERT(scheduler.weight_total_ >= SchedWeight{0});
      DEBUG_ASSERT(scheduler.total_deadline_utilization_ >= SchedUtilization{0});

      UpdateDynamicParams(old_ep, scheduler.virtual_time_);

      // OK, we are done updating this thread's state, as well as most of its
      // scheduler's state.  The last thing to do is to either put the thread
      // back into the proper run queue (if it is READY), or to adjust the
      // preemption time for the scheduler (if this thread is actively running)
      if (thread.state() == THREAD_READY) {
        scheduler.QueueThread(&thread, Placement::Adjustment);
      } else {
        DEBUG_ASSERT(thread.state() == THREAD_RUNNING);
        scheduler.target_preemption_time_ns_ =
            scheduler.start_of_current_time_slice_ns_ + scheduler.ScaleUp(tss.time_slice_ns_);
      }

      // We have made a change to this scheduler's state, we need to trigger a
      // reschedule operation as soon as we can.
      RescheduleMask(cpu_num_to_mask(ss.curr_cpu_));
      scheduler.ValidateInvariants();
    } else {
      // We are dealing with a target which is a non-active thread (it has no
      // scheduler assigned). If the thread is blocked in a wait queue, update
      // its position in the wait queue while also updating its effective
      // profile.  Otherwise, simply update its effective profile.  Once that is
      // all done, update the dynamic parameters of the target using the
      // callback provided by the specific operation.
      SchedulerState::EffectiveProfile old_ep = target.effective_profile();
      if (WaitQueue* wq = target.thread().wait_queue_state().blocking_wait_queue_; wq != nullptr) {
        wq->UpdateBlockedThreadEffectiveProfile(target.thread());
      } else {
        target.RecomputeEffectiveProfile();
      }
      UpdateDynamicParams(old_ep, SchedTime{0});
    }
  } else {
    static_assert(ktl::is_same_v<OwnedWaitQueue, TargetType>);
    SchedulerState::EffectiveProfile old_ep = target.effective_profile();
    target.RecomputeEffectiveProfile();
    UpdateDynamicParams(old_ep, SchedTime{0});
  }
}

void Scheduler::ThreadBaseProfileChanged(Thread& thread) {
  // The base profile of this thread has changed.  While there may or may not be
  // something downstream of this thread, we need to start by dealing with
  // updating this threads static and dynamic scheduling parameters first.
  SchedTime now{CurrentTime()};
  SchedulerState& ss = thread.scheduler_state();
  const bool has_ever_run = thread.state() != thread_state::THREAD_INITIAL;
  ss.effective_profile_.AssertDirtyState(SchedulerState::ProfileDirtyFlag::BaseDirty);

  PiNodeAdapter<Thread> target{thread};
  auto f = [&ss, has_ever_run](const SchedulerState::EffectiveProfile&, SchedTime virt_now) {
    // When the base profile of a thread was changed by a user, we treat it like
    // a yield in order to avoid any attempts by a user to game the system to
    // get more bandwidth by constantly changing the base profile of their
    // thread(s).
    //
    // The exception to this is if the thread has been created, but has never
    // run before.  In this situation, we simply make the thread eligible to run
    // right now.
    const EffectiveProfile& new_ep = ss.effective_profile();
    if (!has_ever_run) {
      ss.start_time_ = SchedTime{0};
      ss.finish_time_ = SchedTime{0};
    } else if (new_ep.IsFair()) {
      ss.start_time_ = virt_now;
      ss.finish_time_ = virt_now;
    } else {
      DEBUG_ASSERT(new_ep.IsDeadline());
      ss.start_time_ = CurrentTime() + new_ep.deadline.deadline_ns;
      ss.finish_time_ = ss.start_time_ + new_ep.deadline.deadline_ns;
    }
    ss.time_slice_ns_ = SchedDuration{0};
  };
  HandlePiInteractionCommon(now, target, f);
}

template <typename TargetType>
void Scheduler::UpstreamThreadBaseProfileChanged(Thread& _upstream, TargetType& _target) {
  // The base profile of a thread upstream of this target node has changed.  We need to
  // do the following:
  //
  // 1) Recompute the target's effective profile.
  // 2) Handle any bookkeeping updates for the scheduler's state, if the target
  //    is a thread which is either RUNNING or READY, and therefore has a
  //    scheduler assigned to it.
  // 3) Handle any updates to the target's dynamic scheduling parameters (eg,
  //    start time, finish time, time slice remaining)
  SchedTime now{CurrentTime()};
  PiNodeAdapter<TargetType> target(_target);
  PiNodeAdapter<Thread> upstream(_upstream);

  if constexpr (ktl::is_same_v<Thread, TargetType>) {
    DEBUG_ASSERT(&_upstream != &_target);
  }

  target.AssertEpDirtyState(SchedulerState::ProfileDirtyFlag::InheritedDirty);
  upstream.AssertEpDirtyState(SchedulerState::ProfileDirtyFlag::Clean);

  auto f = [&target](const SchedulerState::EffectiveProfile&, SchedTime virt_now) {
    // TODO(johngro): What is the proper fair policy here?  Typically, we
    // penalize threads which are changing profiles to make sure there is no way
    // for them to game the system and gain any bandwidth via artificial
    // amplification.  We don't _really_ want to be punishing threads who are
    // having their parameters changed as a result of upstream base profile
    // changes, esp if folks start to allow cross process PI.
    //
    // For now, to keep things simple, we just penalize the target thread the
    // same way that we penalize any other thread.  Basically, don't write code
    // where you block a thread behind another thread and then start to change
    // its profile while blocked.
    const SchedulerState::EffectiveProfile& ep = target.effective_profile();
    if (ep.IsFair()) {
      target.start_time() = virt_now;
      target.finish_time() = virt_now;
      target.time_slice_ns() = SchedDuration{0};
    } else {
      DEBUG_ASSERT(target.effective_profile().IsDeadline());
      target.start_time() = CurrentTime() + ep.deadline.deadline_ns;
      target.finish_time() = target.start_time() + ep.deadline.deadline_ns;
      target.time_slice_ns() = SchedDuration{0};
    }
  };
  HandlePiInteractionCommon(now, target, f);
}

template <typename UpstreamType, typename TargetType>
void Scheduler::JoinNodeToPiGraph(UpstreamType& _upstream, TargetType& _target) {
  SchedTime now{CurrentTime()};
  PiNodeAdapter<TargetType> target(_target);
  PiNodeAdapter<UpstreamType> upstream(_upstream);

  if constexpr (ktl::is_same_v<UpstreamType, TargetType>) {
    DEBUG_ASSERT(&_upstream != &_target);
  }

  target.AssertEpDirtyState(SchedulerState::ProfileDirtyFlag::InheritedDirty);
  upstream.AssertEpDirtyState(SchedulerState::ProfileDirtyFlag::Clean);

  auto f = [&target, &upstream, now](const SchedulerState::EffectiveProfile& target_old_ep,
                                     SchedTime) {
    const SchedulerState::EffectiveProfile& upstream_ep = upstream.effective_profile();
    const SchedulerState::EffectiveProfile& target_new_ep = target.effective_profile();

    // If our upstream node is fair, then we have nothing more to do in the
    // common path.  Our target's effective profile has already been updated
    // appropriately, and no changes to the target's dynamic deadline scheduling
    // parameters needs to be done (since new pressure from a fair thread
    // currently has no effect on deadline utilization).  Any scheduler specific
    // side effects will be handled by the active thread path (below) if the
    // target is an active thread.
    if (upstream_ep.IsFair()) {
      return;
    }

    // Our upstream node is not a fair node, therefore it must be a deadline node.
    // In addition, no matter what it was before, our target node must now be a
    // deadline node.
    DEBUG_ASSERT(upstream_ep.IsDeadline());
    DEBUG_ASSERT(target_new_ep.IsDeadline());

    if (target_old_ep.IsFair()) {
      // If the target has just now become deadline, we can simply transfer the
      // dynamic deadline parameters from upstream to the target.
      target.start_time() = upstream.start_time();
      target.finish_time() = upstream.finish_time();
      target.time_slice_ns() = upstream.time_slice_ns();
    } else {
      // The target was already a deadline thread, then we need to recompute the
      // target's dynamic deadline parameters using the lag equation.
      // Compute the time till absolute deadline (ttad) of the target and
      // upstream threads.
      const SchedDuration target_ttad =
          (target.finish_time() > now) ? (target.finish_time() - now) : SchedDuration{0};
      const SchedDuration upstream_ttad =
          (upstream.finish_time() > now) ? (upstream.finish_time() - now) : SchedDuration{0};
      const SchedDuration combined_ttad = ktl::min(target_ttad, upstream_ttad);

      target.finish_time() = ktl::min(target.finish_time(), upstream.finish_time());
      target.start_time() = target.finish_time() - target_new_ep.deadline.deadline_ns;

      const SchedDuration new_tsr = target.time_slice_ns() + upstream.time_slice_ns() +
                                    (target_new_ep.deadline.utilization * combined_ttad) -
                                    (target_old_ep.deadline.utilization * target_ttad) -
                                    (upstream_ep.deadline.utilization * upstream_ttad);

      // Limit the TSR.  It cannot be less than zero nor can it be more than the
      // time until the absolute deadline of the new combined thread.
      //
      // TODO(johngro): If we did have to clamp the TSR, the amount we clamp by
      // needs to turn into carried lag.
      target.time_slice_ns() = ktl::clamp<SchedDuration>(new_tsr, SchedDuration{0}, combined_ttad);
      target.finish_time() = ktl::min(target.finish_time(), upstream.finish_time());
      target.start_time() = target.finish_time() - target_new_ep.deadline.deadline_ns;
    }
  };
  HandlePiInteractionCommon(now, target, f);
}

template <typename UpstreamType, typename TargetType>
void Scheduler::SplitNodeFromPiGraph(UpstreamType& _upstream, TargetType& _target) {
  SchedTime now{CurrentTime()};
  PiNodeAdapter<TargetType> target(_target);
  PiNodeAdapter<UpstreamType> upstream(_upstream);

  if constexpr (ktl::is_same_v<UpstreamType, TargetType>) {
    DEBUG_ASSERT(&_upstream != &_target);
  }

  target.AssertEpDirtyState(SchedulerState::ProfileDirtyFlag::InheritedDirty);
  upstream.AssertEpDirtyState(SchedulerState::ProfileDirtyFlag::Clean);

  auto f = [&target, &upstream, now](const SchedulerState::EffectiveProfile& target_old_ep,
                                     SchedTime) {
    const SchedulerState::EffectiveProfile& upstream_ep = upstream.effective_profile();
    const SchedulerState::EffectiveProfile& target_new_ep = target.effective_profile();

    // Was the target node a fair node?  If so, there is really nothing for us
    // to do here.
    if (target_old_ep.IsFair()) {
      return;
    }

    DEBUG_ASSERT(target_old_ep.IsDeadline());
    if (target_new_ep.IsFair()) {
      // If target node is now a fair node, then the upstream node must have been
      // a deadline node.  This split operation is what caused the target node to
      // change from deadline to fair, all of the deadline pressure must have been
      // coming from the upstream node.  Assert all of this.
      DEBUG_ASSERT(upstream_ep.IsDeadline());
      DEBUG_ASSERT(target_old_ep.deadline.capacity_ns == upstream_ep.deadline.capacity_ns);
      DEBUG_ASSERT(target_old_ep.deadline.deadline_ns == upstream_ep.deadline.deadline_ns);

      // Give the dynamic deadline parameters over to the upstream node.
      upstream.start_time() = target.start_time();
      upstream.finish_time() = target.finish_time();
      upstream.time_slice_ns() = target.time_slice_ns();

      // Make sure that our fair parameters have been reset.  If we are
      // an active thread, we will now re-arrive with our new parameters.
      target.start_time() = SchedTime{0};
      target.finish_time() = SchedTime{1};
      target.time_slice_ns() = SchedDuration{0};
    } else {
      // OK, the target node is still a deadline node.  If the upstream node
      // is a fair node, we don't have to do anything at all.  A fair node
      // splitting off from a deadline node should not change the deadline
      // node's dynamic parameters.  If the upstream fair node is a thread, it is
      // going to arrive in a new scheduler queue Real Soon Now, and have new
      // dynamic parameters computed for it.
      //
      // If _both_ nodes are deadline nodes, then we need to invoke the lag
      // equation in order to figure out what the new time slice remaining and
      // absolute deadlines are.
      if (upstream_ep.IsDeadline()) {
        // Compute the time till absolute deadline (ttad) of the target.
        const SchedDuration target_ttad =
            (target.finish_time() > now) ? (target.finish_time() - now) : SchedDuration{0};

        // Figure out what the uncapped utilization of the combined thread
        // _would_ have been based on the utilizations of the target and
        // upstream nodes after the split.  It is important when scaling
        // timeslices to be sure that we divide by a utilization value which
        // is the sum of the two (now separated) utilization values.
        const SchedUtilization combined_uncapped_utilization =
            target_new_ep.deadline.utilization + upstream_ep.deadline.utilization;

        // If the upstream node's time till absolute deadline is zero, there
        // is no need to compute its time slice remaining right now; we
        // would just end up capping it to zero anyway.
        //
        // TODO(johngro): this changes when carried lag comes into the picture.
        if (upstream.finish_time() <= now) {
          upstream.time_slice_ns() = SchedDuration{0};
        } else {
          // Looks like we need to compute this value after all.
          const SchedDuration upstream_ttad = upstream.finish_time() - now;
          const SchedDuration new_upstream_tsr =
              upstream_ep.deadline.utilization *
              ((target.time_slice_ns() / combined_uncapped_utilization) + upstream_ttad -
               target_ttad);

          // TODO(johngro): This also changes when carried lag comes into
          // play.
          upstream.time_slice_ns() = ktl::max(new_upstream_tsr, SchedDuration{0});
        }

        // TODO(johngro): Fix this.  Logically, it is not correct to
        // preserve the abs deadline of the target after the split.  The
        // target's bookkeeping should be equivalent to the values which
        // would be obtained by joining all of the threads which exist
        // upstream of this node together.  Because of this, our new target
        // finish time should be equal to the min across all finish times
        // immediately upstream of this node.
        //
        // Now handle the target node.  We preserve the absolute deadline of
        // the target node before and after the split, so we need to
        // recompute its start time so that the distance between the
        // absolute deadline and the start time is equal to the new relative
        // deadline of the target.
        target.start_time() = target.finish_time() - target_new_ep.deadline.deadline_ns;

        // The time till absolute deadline of the pre and post split target
        // remains the same, so the ttad contributions to the timeslice
        // remaining simply drop out of the lag equation.
        //
        // Note that fixed point division takes the precision of the
        // assignee into account to provide headroom in certain situations.
        // Use an intermediate with the same fractional precision as the
        // utilization operands before scaling the non-fractional timeslice.
        const SchedUtilization utilization_ratio =
            target_new_ep.deadline.utilization / combined_uncapped_utilization;
        const SchedDuration new_target_tsr = target.time_slice_ns() * utilization_ratio;

        // TODO(johngro): once again, need to consider carried lag here.
        target.time_slice_ns() = ktl::max(new_target_tsr, SchedDuration{0});
      }
    }
  };
  HandlePiInteractionCommon(now, target, f);
}

template void Scheduler::UpstreamThreadBaseProfileChanged(Thread& upstream, Thread& target);
template void Scheduler::UpstreamThreadBaseProfileChanged(Thread& upstream, OwnedWaitQueue& target);

template void Scheduler::JoinNodeToPiGraph(Thread& upstream, Thread& target);
template void Scheduler::JoinNodeToPiGraph(Thread& upstream, OwnedWaitQueue& target);
template void Scheduler::JoinNodeToPiGraph(OwnedWaitQueue& upstream, Thread& target);
template void Scheduler::JoinNodeToPiGraph(OwnedWaitQueue& upstream, OwnedWaitQueue& target);

template void Scheduler::SplitNodeFromPiGraph(Thread& upstream, Thread& target);
template void Scheduler::SplitNodeFromPiGraph(Thread& upstream, OwnedWaitQueue& target);
template void Scheduler::SplitNodeFromPiGraph(OwnedWaitQueue& upstream, Thread& target);
template void Scheduler::SplitNodeFromPiGraph(OwnedWaitQueue& upstream, OwnedWaitQueue& target);
