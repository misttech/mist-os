// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "kernel/owned_wait_queue.h"

#include <lib/counters.h>
#include <lib/fit/defer.h>
#include <zircon/compiler.h>

#include <arch/mp.h>
#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <fbl/enum_bits.h>
#include <kernel/auto_preempt_disabler.h>
#include <kernel/scheduler.h>
#include <kernel/wait_queue_internal.h>
#include <ktl/algorithm.h>
#include <ktl/bit.h>
#include <ktl/type_traits.h>
#include <object/thread_dispatcher.h>

#include <ktl/enforce.h>

// Notes on the defined kernel counters.
//
// Adjustments (aka promotions and demotions)
// The number of times that a thread either gained or lost inherited profile
// pressure as a result of a PI event.
//
// Note that the number of promotions does not have to equal the number of
// demotions in the system.  For example, a thread could slowly gain weight as
// fair scheduled threads join a wait queue it owns, then suddenly drop
// back down to its base profile when the thread releases ownership of the
// queue.
//
// In addition to simple promotions and demotions, the number of threads whose
// effective profile changed as a result of another thread's base profile
// changing is also tracked, although whether these changes amount to a
// promotion or a demotion is not computed.
//
// Max chain traversal.
// The maximum traversed length of a PI chain during execution of the propagation
// algorithm.
//
// The length of a propagation chain is defined as the number of nodes in an
// inheritance graph which are affected by a propagation event.  For example, if
// a thread (T1) blocks in an owned wait queue (OWQ1), adding an edge between
// them, and the wait queue has no owner, then the propagation event's chain
// length is 1. This is regardless of whether or not the blocking thread
// currently owns one or more wait queues upstream from it. The OWQ1's IPVs were
// updated, but no other nodes in the graph were affected.  If the OWQ1 had been
// owned by a running/runnable thread (T2), then the chain length of the
// operation would have been two instead, since both OWQ1 and T2 needed to be
// visited and updated.
KCOUNTER(pi_promotions, "kernel.pi.adj.promotions")
KCOUNTER(pi_demotions, "kernel.pi.adj.demotions")
KCOUNTER(pi_bp_changed, "kernel.pi.adj.bp_changed")
KCOUNTER_DECLARE(max_pi_chain_traverse, "kernel.pi.max_chain_traverse", Max)

namespace {

enum class ChainLengthTrackerOpt : uint32_t {
  None = 0,
  RecordMaxLength = 1,
  EnforceLengthGuard = 2,
};
FBL_ENABLE_ENUM_BITS(ChainLengthTrackerOpt)

// By default, we always maintain the max length counter, and we enforce the
// length guard in everything but release builds.
constexpr ChainLengthTrackerOpt kEnablePiChainGuards =
    ((LK_DEBUGLEVEL > 0) ? ChainLengthTrackerOpt::EnforceLengthGuard : ChainLengthTrackerOpt::None);

constexpr ChainLengthTrackerOpt kDefaultChainLengthTrackerOpt =
    ChainLengthTrackerOpt::RecordMaxLength | kEnablePiChainGuards;

template <ChainLengthTrackerOpt Options = kDefaultChainLengthTrackerOpt>
class ChainLengthTracker {
 public:
  using Opt = ChainLengthTrackerOpt;

  ChainLengthTracker() {
    if constexpr (Options != Opt::None) {
      nodes_visited_ = 0;
    }
  }

  ~ChainLengthTracker() {
    if constexpr ((Options & Opt::EnforceLengthGuard) != Opt::None) {
      // Note, the only real reason that this is an accurate max at all is
      // because the counter is effectively protected by the thread lock
      // (although there is no real good way to annotate that fact).  When we
      // finally remove the thread lock, we are going to need to do better than
      // this.
      auto old = max_pi_chain_traverse.ValueCurrCpu();
      if (nodes_visited_ > old) {
        max_pi_chain_traverse.Set(nodes_visited_);
      }
    }
  }

  void NodeVisited() {
    if constexpr (Options != Opt::None) {
      ++nodes_visited_;
    }

    if constexpr ((Options & Opt::EnforceLengthGuard) != Opt::None) {
      constexpr uint32_t kMaxChainLen = 2048;
      ASSERT_MSG(nodes_visited_ <= kMaxChainLen, "visited %u", nodes_visited_);
    }
  }

 private:
  uint32_t nodes_visited_ = 0;
};

using AddSingleEdgeTag = decltype(OwnedWaitQueue::AddSingleEdgeOp);
using RemoveSingleEdgeTag = decltype(OwnedWaitQueue::RemoveSingleEdgeOp);
using BaseProfileChangedTag = decltype(OwnedWaitQueue::BaseProfileChangedOp);

inline bool IpvsAreConsequential(const SchedulerState::InheritedProfileValues* ipvs) {
  return (ipvs != nullptr) && ((ipvs->total_weight != SchedWeight{0}) ||
                               (ipvs->uncapped_utilization != SchedUtilization{0}));
}

template <typename UpstreamType, typename DownstreamType>
void Propagate(UpstreamType& upstream, DownstreamType& downstream, AddSingleEdgeTag)
    TA_REQ(thread_lock, preempt_disabled_token) {
  Scheduler::JoinNodeToPiGraph(upstream, downstream);
  if constexpr (ktl::is_same_v<DownstreamType, Thread>) {
    pi_promotions.Add(1u);
  }
}

template <typename UpstreamType, typename DownstreamType>
void Propagate(UpstreamType& upstream, DownstreamType& downstream, RemoveSingleEdgeTag)
    TA_REQ(thread_lock, preempt_disabled_token) {
  Scheduler::SplitNodeFromPiGraph(upstream, downstream);
  if constexpr (ktl::is_same_v<DownstreamType, Thread>) {
    pi_demotions.Add(1u);
  }
}

template <typename UpstreamType, typename DownstreamType>
void Propagate(UpstreamType& upstream, DownstreamType& downstream, BaseProfileChangedTag)
    TA_REQ(thread_lock, preempt_disabled_token) {
  Scheduler::UpstreamThreadBaseProfileChanged(upstream, downstream);
  if constexpr (ktl::is_same_v<DownstreamType, Thread>) {
    pi_bp_changed.Add(1u);
  }
}

}  // namespace

OwnedWaitQueue::~OwnedWaitQueue() {
  // Something is very very wrong if we have been allowed to destruct while we
  // still have an owner.
  DEBUG_ASSERT(owner_ == nullptr);
}

void OwnedWaitQueue::DisownAllQueues(Thread* t) {
  // It is important that this thread not be blocked by any other wait queues
  // during this operation.  If it was possible for the thread to be blocked,
  // we would need to update all of the PI chain bookkeeping too.
  DEBUG_ASSERT(t->wait_queue_state_.blocking_wait_queue_ == nullptr);

  for (auto& q : t->wait_queue_state_.owned_wait_queues_) {
    DEBUG_ASSERT(q.owner_ == t);
    q.owner_ = nullptr;
  }

  t->wait_queue_state_.owned_wait_queues_.clear();
}

void OwnedWaitQueue::WakeThreadsInternal(uint32_t wake_count, zx_time_t now,
                                         Hook on_thread_wake_hook) {
  DEBUG_ASSERT(magic() == kOwnedMagic);
  auto post_op_validate = fit::defer([this]() { ValidateSchedStateStorage(); });

  // Start by removing any existing owner.  We will either select a new owner
  // based on what the user-provided hook tells us to do, or we should end up
  // with no owner.
  AssignOwnerInternal(nullptr);

  uint32_t woken = 0;
  while (woken < wake_count) {
    // Consider the thread that the queue considers to be the most important to
    // wake right now.  If there are no threads left in the queue, then we are
    // done.
    Thread* t = Peek(now);
    if (t == nullptr) {
      break;
    }

    // Call the user supplied hook and let them decide what to do with this
    // thread (updating their own bookkeeping in the process)
    using Action = Hook::Action;
    Action action = on_thread_wake_hook(t);

    // If we should stop, just return.  We are done.
    if (action == Action::Stop) {
      break;
    }

    // All other choices involve waking up this thread, so go ahead and do that now.
    ++woken;

    // If this is a wake-and-assign-owner operation, then the number of threads
    // we have woken so far should be exactly one.
    DEBUG_ASSERT((action != Action::SelectAndAssignOwner) || (woken == 1));

    // Dequeue the thread and propagate the PI consequences.  Do not actually unblock the
    // thread from the scheduler's perspective just yet.
    DequeueThread(t, ZX_OK);
    UpdateSchedStateStorageThreadRemoved(*t);
    BeginPropagate(*t, *this, RemoveSingleEdgeOp);

    // No go ahead and unblock the thread we just removed from the wait queue.
    //
    // TODO(johngro) : instead of unblocking the thread now, it might be better
    // to put it on a local list, then batch unblock all of the threads we woke
    // at the end of everything.
    Scheduler::Unblock(t);

    // If this thread was selected to become the new queue owner, and it still
    // has waiters, make the assignment and we are done.
    if (action == Action::SelectAndAssignOwner) {
      if (!IsEmpty()) {
        AssignOwnerInternal(t);
      }
      break;
    }

    // Looks like this is just a simple wake operation.  Keep going as long as
    // there are more threads to wake.
    DEBUG_ASSERT(action == Action::SelectAndKeepGoing);
  };

  // If there are no threads left waiting in this queue, then it cannot have any owner.
  DEBUG_ASSERT((owner_ == nullptr) || !IsEmpty());
}

void OwnedWaitQueue::ValidateSchedStateStorageUnconditional() {
  thread_lock.AssertHeld();
  if (inherited_scheduler_state_storage_ != nullptr) {
    bool found = false;
    for (const Thread& t : this->collection_.threads()) {
      if (&t.wait_queue_state().inherited_scheduler_state_storage_ ==
          inherited_scheduler_state_storage_) {
        found = true;
        break;
      }
    }
    DEBUG_ASSERT(found);
  } else {
    DEBUG_ASSERT(this->IsEmpty());
  }
}

SchedulerState::InheritedProfileValues OwnedWaitQueue::SnapshotThreadIpv(Thread& thread) {
  const SchedulerState& tss = thread.scheduler_state();
  SchedulerState::InheritedProfileValues ret = tss.inherited_profile_values_;
  const SchedulerState::BaseProfile& bp = tss.base_profile_;

  if (bp.inheritable) {
    if (bp.discipline == SchedDiscipline::Fair) {
      ret.total_weight += bp.fair.weight;
    } else {
      DEBUG_ASSERT(ret.min_deadline != SchedDuration{0});
      ret.uncapped_utilization += bp.deadline.utilization;
      ret.min_deadline = ktl::min(ret.min_deadline, bp.deadline.deadline_ns);
    }
  }

  return ret;
}

void OwnedWaitQueue::ApplyIpvDeltaToThread(const SchedulerState::InheritedProfileValues* old_ipv,
                                           const SchedulerState::InheritedProfileValues* new_ipv,
                                           Thread& thread) {
  DEBUG_ASSERT((old_ipv != nullptr) || (new_ipv != nullptr));

  SchedWeight weight_delta = new_ipv ? new_ipv->total_weight : SchedWeight{0};
  SchedUtilization util_delta = new_ipv ? new_ipv->uncapped_utilization : SchedUtilization{0};
  if (old_ipv != nullptr) {
    weight_delta -= old_ipv->total_weight;
    util_delta -= old_ipv->uncapped_utilization;
  }

  SchedulerState& tss = thread.scheduler_state();
  SchedulerState::InheritedProfileValues& thread_ipv = tss.inherited_profile_values_;

  tss.effective_profile_.MarkInheritedProfileChanged();
  thread_ipv.total_weight += weight_delta;
  thread_ipv.uncapped_utilization += util_delta;

  DEBUG_ASSERT(thread_ipv.total_weight >= SchedWeight{0});
  DEBUG_ASSERT(thread_ipv.uncapped_utilization >= SchedUtilization{0});

  // If a set of IPVs is going away, and the value which is going away was the
  // minimum, then we need to recompute the new minimum by checking the
  // minimum across all of this thread's owned wait queues.
  //
  // TODO(johngro): Consider keeping the set of owned wait queues as a WAVL
  // tree, indexed by minimum relative deadline, so that this can be
  // maintained in O(1) time instead of O(N).
  if ((new_ipv != nullptr) && (new_ipv->min_deadline <= thread_ipv.min_deadline)) {
    thread_ipv.min_deadline = ktl::min(thread_ipv.min_deadline, new_ipv->min_deadline);
  } else {
    if ((old_ipv != nullptr) && (old_ipv->min_deadline <= thread_ipv.min_deadline)) {
      SchedDuration new_min_deadline{SchedDuration::Max()};

      for (auto& other_queue : thread.wait_queue_state().owned_wait_queues_) {
        if (!other_queue.IsEmpty()) {
          DEBUG_ASSERT(other_queue.inherited_scheduler_state_storage_ != nullptr);
          const SchedulerState::InheritedProfileValues& other_ipvs =
              other_queue.inherited_scheduler_state_storage_->ipvs;
          new_min_deadline = ktl::min(new_min_deadline, other_ipvs.min_deadline);
        }
      }

      thread_ipv.min_deadline = new_min_deadline;
    }

    if (new_ipv != nullptr) {
      thread_ipv.min_deadline = ktl::min(thread_ipv.min_deadline, new_ipv->min_deadline);
    }
  }

  DEBUG_ASSERT(thread_ipv.min_deadline > SchedDuration{0});
}

void OwnedWaitQueue::ApplyIpvDeltaToOwq(const SchedulerState::InheritedProfileValues* old_ipv,
                                        const SchedulerState::InheritedProfileValues* new_ipv,
                                        OwnedWaitQueue& owq) {
  SchedWeight weight_delta = new_ipv ? new_ipv->total_weight : SchedWeight{0};
  SchedUtilization util_delta = new_ipv ? new_ipv->uncapped_utilization : SchedUtilization{0};

  if (old_ipv != nullptr) {
    weight_delta -= old_ipv->total_weight;
    util_delta -= old_ipv->uncapped_utilization;
  }

  DEBUG_ASSERT(!owq.IsEmpty());
  DEBUG_ASSERT(owq.inherited_scheduler_state_storage_ != nullptr);
  SchedulerState::WaitQueueInheritedSchedulerState& iss = *owq.inherited_scheduler_state_storage_;

  iss.ipvs.total_weight += weight_delta;
  iss.ipvs.uncapped_utilization += util_delta;
  iss.ipvs.min_deadline = owq.collection_.MinInheritableRelativeDeadline();

  DEBUG_ASSERT(iss.ipvs.total_weight >= SchedWeight{0});
  DEBUG_ASSERT(iss.ipvs.uncapped_utilization >= SchedUtilization{0});
  DEBUG_ASSERT(iss.ipvs.min_deadline > SchedDuration{0});
}

bool OwnedWaitQueue::CheckForCycle(const OwnedWaitQueue* owq, const Thread* owner_thread,
                                   const Thread* blocking_thread) {
  // If the owner of OWQ is being set to nullptr, then there cannot be a cycle.
  if (owner_thread == nullptr) {
    return false;
  }

  // ASSERT if this operation goes on for way too long, but do not record this
  // as a chain traversal for kcounter purposes.
  ChainLengthTracker<kEnablePiChainGuards> inf_loop_guard;

  // Trace the downstream path from the proposed owner thread.  If it leads back
  // to either |owq| or |blocking_thread|, then we would have a cycle if
  // |blocking_thread| blocked in |owq|, and |owner_thread| were to become the
  // owner.
  const Thread* thread_iter = owner_thread;

  while (true) {
    // Peek at the wait queue which is blocking this thread.
    const OwnedWaitQueue* next_owq =
        DowncastToOwq(thread_iter->wait_queue_state().blocking_wait_queue_);

    // If there is no blocking wait queue, or it is not an owned wait queue,
    // then there is no cycle and we are finished.
    if (next_owq == nullptr) {
      return false;
    }
    inf_loop_guard.NodeVisited();

    // If the OWQ blocking this thread is the OWQ involved in cycle detection,
    // then we have found a cycle.
    if (next_owq == owq) {
      return true;
    }

    // Move on to the next thread.  If there is no next thread, then there is no
    // cycle.  Alternatively, if we are considering blocking a thread, and this
    // is the same thread as the OWQ's owner, then we have detected a cycle.
    thread_iter = next_owq->owner_;
    if (thread_iter == nullptr) {
      return false;
    }
    inf_loop_guard.NodeVisited();

    if (thread_iter == blocking_thread) {
      return true;
    }
  }
}

template <OwnedWaitQueue::PropagateOp OpType>
void OwnedWaitQueue::BeginPropagate(Thread& upstream_node, OwnedWaitQueue& downstream_node,
                                    PropagateOpTag<OpType> op) {
  // When needed, base profile changes will directly call FinishPropagate.
  static_assert(OpType != PropagateOp::BaseProfileChanged);
  SchedulerState::InheritedProfileValues ipv_snapshot;

  // Are we starting from a thread during an edge remove operation?  If so,
  // and we were the last thread to leave the queue, then there is no longer
  // any IPV storage for our downstream wait queue which needs to be updated.
  // If the wait queue has no owner either, then we are done with propagation.
  if constexpr (OpType == PropagateOp::RemoveSingleEdge) {
    if (downstream_node.IsEmpty() && (downstream_node.owner_ == nullptr)) {
      return;
    }
  }

  ipv_snapshot = SnapshotThreadIpv(upstream_node);

  if constexpr (OpType == PropagateOp::RemoveSingleEdge) {
    FinishPropagate(upstream_node, downstream_node, nullptr, &ipv_snapshot, op);
  } else if constexpr (OpType == PropagateOp::AddSingleEdge) {
    FinishPropagate(upstream_node, downstream_node, &ipv_snapshot, nullptr, op);
  }
}

template <OwnedWaitQueue::PropagateOp OpType>
void OwnedWaitQueue::BeginPropagate(OwnedWaitQueue& upstream_node, Thread& downstream_node,
                                    PropagateOpTag<OpType> op) {
  // When needed, base profile changes will directly call FinishPropagate.
  static_assert(OpType != PropagateOp::BaseProfileChanged);

  if constexpr (OpType == PropagateOp::AddSingleEdge) {
    // If we are adding an owner to this OWQ, we should be able to assert that
    // it does not currently have one.
    DEBUG_ASSERT(upstream_node.owner_ == nullptr);
    DEBUG_ASSERT(!upstream_node.InContainer());

    upstream_node.owner_ = &downstream_node;
    downstream_node.wait_queue_state().owned_wait_queues_.push_back(&upstream_node);
  } else if constexpr (OpType == PropagateOp::RemoveSingleEdge) {
    // If we are removing the owner of this OWQ, or we are updating the base
    // profile of the immediately upstream thread, we should be able to assert
    // that the owq's current owner is the thread passed to this method.
    DEBUG_ASSERT(upstream_node.owner_ == &downstream_node);
    DEBUG_ASSERT(upstream_node.InContainer());
    downstream_node.wait_queue_state().owned_wait_queues_.erase(upstream_node);
    upstream_node.owner_ = nullptr;
  }

  // If the OWQ we are starting from has no active waiters, then there are no
  // IPV deltas to propagate.  After updating the links, we are finished.
  if (upstream_node.IsEmpty()) {
    return;
  }

  DEBUG_ASSERT(upstream_node.inherited_scheduler_state_storage_ != nullptr);
  SchedulerState::InheritedProfileValues& ipvs =
      upstream_node.inherited_scheduler_state_storage_->ipvs;

  if constexpr (OpType == PropagateOp::RemoveSingleEdge) {
    FinishPropagate(upstream_node, downstream_node, nullptr, &ipvs, op);
  } else if constexpr (OpType == PropagateOp::AddSingleEdge) {
    FinishPropagate(upstream_node, downstream_node, &ipvs, nullptr, op);
  }
}

template <OwnedWaitQueue::PropagateOp OpType, typename UpstreamNodeType,
          typename DownstreamNodeType>
void OwnedWaitQueue::FinishPropagate(UpstreamNodeType& upstream_node,
                                     DownstreamNodeType& downstream_node,
                                     const SchedulerState::InheritedProfileValues* added_ipv,
                                     const SchedulerState::InheritedProfileValues* lost_ipv,
                                     PropagateOpTag<OpType> op) {
  // Propagation must start from a(n) (OWQ|Thread) and proceed to a(n) (Thread|OWQ).
  static_assert((ktl::is_same_v<UpstreamNodeType, OwnedWaitQueue> &&
                 ktl::is_same_v<DownstreamNodeType, Thread>) ||
                    (ktl::is_same_v<UpstreamNodeType, Thread> &&
                     ktl::is_same_v<DownstreamNodeType, OwnedWaitQueue>),
                "Bad types for FinishPropagate.  Must be either OWQ -> Thread, or Thread -> OWQ");

  constexpr bool kStartingFromThread = ktl::is_same_v<UpstreamNodeType, Thread>;

  // If neither the IPVs we are adding, nor the IPVs we are removing, are
  // "consequential" (meaning, the have either some fair weight, or some
  // deadline capacity, or both), then we can just get out now.  There are no
  // effective changes to propagate.
  if (!IpvsAreConsequential(added_ipv) && !IpvsAreConsequential(lost_ipv)) {
    return;
  }

  // Set up the pointers we will use as iterators for traversing the inheritance
  // graph.  Snapshot the starting node's current inherited profile values which
  // we need to propagate.
  OwnedWaitQueue* owq_iter;
  Thread* thread_iter;

  if constexpr (kStartingFromThread) {
    thread_iter = &upstream_node;
    owq_iter = &downstream_node;

    // Is this a base profile changed operation?  If so, we should already have
    // a link between our thread and the downstream owned wait queue.  Go ahead
    // and ASSERT this.  We don't need to bother to check the other
    // combinations; those have already been asserted during
    // BeginPropagate.
    if constexpr (OpType == PropagateOp::BaseProfileChanged) {
      DEBUG_ASSERT_MSG(
          thread_iter->wait_queue_state().blocking_wait_queue_ == static_cast<WaitQueue*>(owq_iter),
          "blocking wait queue %p owq_iter %p",
          thread_iter->wait_queue_state().blocking_wait_queue_, static_cast<WaitQueue*>(owq_iter));
    }
  } else {
    // Base profile changes should never start from OWQs.
    static_assert(OpType != PropagateOp::BaseProfileChanged);
    owq_iter = &upstream_node;
    thread_iter = &downstream_node;
    DEBUG_ASSERT(!owq_iter->IsEmpty());
  }

  if constexpr (OpType == PropagateOp::AddSingleEdge) {
    DEBUG_ASSERT(added_ipv != nullptr);
    DEBUG_ASSERT(lost_ipv == nullptr);
  } else if constexpr (OpType == PropagateOp::RemoveSingleEdge) {
    DEBUG_ASSERT(added_ipv == nullptr);
    DEBUG_ASSERT(lost_ipv != nullptr);
  } else if constexpr (OpType == PropagateOp::BaseProfileChanged) {
    static_assert(
        kStartingFromThread,
        "Base profile propagation changes may only start from Threads, not OwnedWaitQueues");
    DEBUG_ASSERT(added_ipv != nullptr);
    DEBUG_ASSERT(lost_ipv != nullptr);
  } else {
    static_assert(OpType != OpType, "Unrecognized propagation operation");
  }

  // When we have finally finished updating everything, make sure to update
  // our max traversal statistic.
  ChainLengthTracker len_tracker;

  // OK - we are finally ready to get to work.  Use a slightly-evil(tm) goto in
  // order to start our propagate loop with the proper phase (either
  // thread-to-OWQ first, or OWQ-to-thread first)
  if constexpr (kStartingFromThread == false) {
    goto start_from_owq;
  } else if constexpr (OpType == PropagateOp::RemoveSingleEdge) {
    // Are we starting from a thread during an edge remove operation?  If so,
    // and if we were the last thread to leave our wait queue, then we don't
    // need to bother to update its IPVs anymore (it cannot have any IPVs if it
    // has no waiters), so we can just skip it an move on to its owner thread.
    //
    // Additionally, we know that it must have an owner thread at this point in
    // time.  If if didn't, BeginPropagate would have already bailed out.
    if (owq_iter->IsEmpty()) {
      thread_iter = owq_iter->owner_;
      DEBUG_ASSERT(thread_iter != nullptr);
      goto start_from_owq;
    }
  }

  while (true) {
    {
      // Propagate from the current thread_iter to the current owq_iter.
      // First, apply the change in pressure to the next OWQ in the chain.
      ApplyIpvDeltaToOwq(lost_ipv, added_ipv, *owq_iter);

      // We should not be here if this OWQ has no waiters.  That special case
      // was handled above.
      DEBUG_ASSERT(owq_iter->Count() > 0);

      // If our OWQ target has exactly one waiter, then propagation of the
      // dynamic parameters is simple, we just need to copy that thread's current
      // dynamic parameters.  Otherwise, we call into the scheduler in order to
      // allow it to apply the lag equation, as appropriate.
      if (owq_iter->Count() == 1) {
        DEBUG_ASSERT(owq_iter->inherited_scheduler_state_storage_ != nullptr);
        SchedulerState::WaitQueueInheritedSchedulerState& owq_iss =
            *owq_iter->inherited_scheduler_state_storage_;

        Thread& only_waiter = owq_iter->collection_.PeekOnlyThread();
        SchedulerState& only_waiter_ss = only_waiter.scheduler_state();

        owq_iss.start_time = only_waiter_ss.start_time_;
        owq_iss.finish_time = only_waiter_ss.finish_time_;
        owq_iss.time_slice_ns = only_waiter_ss.time_slice_ns_;
      } else {
        Propagate(upstream_node, *owq_iter, op);
      }
      len_tracker.NodeVisited();

      // Advance to the next thread, if any.  If there isn't another thread,
      // then we are finished, simply break out of the propagation loop.
      thread_iter = owq_iter->owner_;
      if (thread_iter == nullptr) {
        break;
      }
    }

    // clang-format off
    [[maybe_unused]] start_from_owq:
    // clang-format on

    {
      // Propagate from the current owq_iter to the current thread_iter.
      // Apply the change in pressure to the next thread in the chain.
      ApplyIpvDeltaToThread(lost_ipv, added_ipv, *thread_iter);
      Propagate(upstream_node, *thread_iter, op);
      len_tracker.NodeVisited();

      owq_iter = DowncastToOwq(thread_iter->wait_queue_state().blocking_wait_queue_);
      if (owq_iter == nullptr) {
        break;
      }
    }
  }
}

void OwnedWaitQueue::SetThreadBaseProfileAndPropagate(Thread& thread,
                                                      const SchedulerState::BaseProfile& profile) {
  AutoEagerReschedDisabler eager_resched_disabler;
  SchedulerState& state = thread.scheduler_state();

  SchedulerState::InheritedProfileValues old_ipvs;
  OwnedWaitQueue* owq = DowncastToOwq(thread.wait_queue_state().blocking_wait_queue_);

  // If our thread is blocked in an owned wait queue, we need observe the
  // thread's transmitted IPVs before and after the base profile change in order
  // to properly handle propagation.
  if (owq != nullptr) {
    old_ipvs = SnapshotThreadIpv(thread);
  }

  // Regardless of the state of the thread whose base profile has changed, we
  // need to update the base profile and let the scheduler know.  The scheduler
  // code will handle:
  // 1) Updating our effective profile
  // 2) Repositioning us in our wait queue (if we are blocked)
  // 3) Updating our dynamic scheduling parameters (if we are either runnable or
  //    a blocked deadline thread)
  // 4) Updating the scheduler's state (if we happen to be a runnable thread).
  state.base_profile_ = profile;
  state.effective_profile_.MarkBaseProfileChanged();
  Scheduler::ThreadBaseProfileChanged(thread);

  // Now, if we are blocked in an owned wait queue, propagate the consequences
  // of the base profile change downstream.
  if (owq != nullptr) {
    SchedulerState::InheritedProfileValues new_ipvs = SnapshotThreadIpv(thread);
    FinishPropagate(thread, *owq, &new_ipvs, &old_ipvs, BaseProfileChangedOp);
  }
}

zx_status_t OwnedWaitQueue::AssignOwner(Thread* new_owner) {
  DEBUG_ASSERT(magic() == kOwnedMagic);

  // If there is a change of owners, and the change would produce a cycle, then
  // fail the call instead.
  if ((owner_ != new_owner) && CheckForCycle(this, new_owner)) {
    return ZX_ERR_BAD_STATE;
  }

  AssignOwnerInternal(new_owner);
  return ZX_OK;
}

void OwnedWaitQueue::AssignOwnerInternal(Thread* new_owner) {
  // If there is no change, then we are done already.
  if (owner_ == new_owner) {
    return;
  }

  // Start by releasing the old owner (if any) and propagating the PI effects.
  if (owner_ != nullptr) {
    BeginPropagate(*this, *owner_, RemoveSingleEdgeOp);
  }

  // If there is a new owner to assign, do so now and propagate the PI effects.
  if (new_owner != nullptr) {
    BeginPropagate(*this, *new_owner, AddSingleEdgeOp);
  }

  ValidateSchedStateStorage();
}

zx_status_t OwnedWaitQueue::BlockAndAssignOwner(const Deadline& deadline, Thread* new_owner,
                                                ResourceOwnership resource_ownership,
                                                Interruptible interruptible) {
  Thread* current_thread = Thread::Current::Get();

  DEBUG_ASSERT(magic() == kOwnedMagic);
  DEBUG_ASSERT(current_thread->state() == THREAD_RUNNING);
  thread_lock.AssertHeld();

  // TODO(johngro) : when we start to use distributed locking, start by
  // obtaining the queue lock for the current thread, followed by the queue lock
  // for this OWQ.  Then validate that the operation is still technically legal.
  if (CheckForCycle(this, new_owner, current_thread)) {
    // TODO(johngro) : Change this when we reach the point that we are ready to
    // propagate errors in the case that someone attempts to create a cycle.
    // For now, we avoid the cycle by forcing the OWQ to become unowned.
#if 0
    return ZX_ERR_BAD_STATE;
#else
    new_owner = nullptr;
#endif
  }

  // If the there is a change of owner happening, start by releasing the current
  // owner.
  const bool owner_changed = (owner_ != new_owner);
  if (owner_changed) {
    AssignOwnerInternal(nullptr);
  }

  // Perform the first half of the BlockEtc operation.  This will attempt to add
  // an edge between the thread which is blocking, and the OWQ it is blocking in
  // (this).  We know that this cannot produce a cycle in the graph because we
  // know that this OWQ does not currently have an owner.
  //
  // If the block preamble fails, then the state of the actual wait queue is
  // unchanged and we can just get out now.
  zx_status_t res = BlockEtcPreamble(deadline, 0u, resource_ownership, interruptible);
  if (res != ZX_OK) {
    // There are only three reasons why the pre-wait operation should ever fail.
    //
    // 1) ZX_ERR_TIMED_OUT            : The timeout has already expired.
    // 2) ZX_ERR_INTERNAL_INTR_KILLED : The thread has been signaled for death.
    // 3) ZX_ERR_INTERNAL_INTR_RETRY  : The thread has been signaled for suspend.
    //
    // No matter what, we are not actually going to block in the wait queue.
    // Even so, however, we still need to assign the owner to what was
    // requested by the thread.  Just because we didn't manage to block does
    // not mean that ownership assignment gets skipped.
    ZX_DEBUG_ASSERT((res == ZX_ERR_TIMED_OUT) || (res == ZX_ERR_INTERNAL_INTR_KILLED) ||
                    (res == ZX_ERR_INTERNAL_INTR_RETRY));
    if (owner_changed) {
      AssignOwnerInternal(new_owner);
    }

    return res;
  }

  // We succeeded in placing our thread into our wait collection.  Make sure we
  // update the scheduler state storage location if needed, then propagate the
  // effects down the chain.
  UpdateSchedStateStorageThreadAdded(*current_thread);
  BeginPropagate(*current_thread, *this, AddSingleEdgeOp);

  // Finally, assign the new owner (if we have one).
  if (owner_changed && (new_owner != nullptr)) {
    DEBUG_ASSERT(owner_ == nullptr);
    AssignOwnerInternal(new_owner);
  }

  // Finally, go ahead and run the second half of the BlockEtc operation.
  // This will actually block our thread and handle setting any timeout timers
  // in the process.

  // DANGER!! DANGER!! DANGER!! DANGER!! DANGER!! DANGER!! DANGER!! DANGER!!
  //
  // It is very important that no attempts to access |this| are made after
  // either of the calls to BlockEtcPostamble (below). When someone eventually
  // comes along and unblocks us from the queue, they have already taken care of
  // removing us from the this wait queue.  It it totally possible that the wait
  // queue we were blocking in has been destroyed by the time we make it out of
  // BlockEtcPostable, making |this| no longer a valid pointer.
  //
  // DANGER!! DANGER!! DANGER!! DANGER!! DANGER!! DANGER!! DANGER!! DANGER!!
  res = BlockEtcPostamble(deadline);

  return res;
}

void OwnedWaitQueue::WakeThreads(uint32_t wake_count, Hook on_thread_wake_hook) {
  DEBUG_ASSERT(magic() == kOwnedMagic);
  zx_time_t now = current_time();

  WakeThreadsInternal(wake_count, now, on_thread_wake_hook);
}

void OwnedWaitQueue::WakeAndRequeue(uint32_t wake_count, OwnedWaitQueue* requeue_target,
                                    uint32_t requeue_count, Thread* requeue_owner,
                                    Hook on_thread_wake_hook, Hook on_thread_requeue_hook) {
  DEBUG_ASSERT(magic() == kOwnedMagic);
  DEBUG_ASSERT(requeue_target != nullptr);
  DEBUG_ASSERT(requeue_target->magic() == kOwnedMagic);
  zx_time_t now = current_time();

  auto post_op_validate = fit::defer([this]() { ValidateSchedStateStorage(); });

  // If the potential new owner of the requeue wait queue is already dead,
  // then it cannot become the owner of the requeue wait queue.
  if (requeue_owner != nullptr) {
    // It should not be possible for a thread which is not yet running to be
    // declared as the owner of an OwnedWaitQueue.  Any attempts to assign
    // ownership to a thread which is not yet started should have been rejected
    // by layers of code above us, and a proper status code returned to the
    // user.
    DEBUG_ASSERT(requeue_owner->state() != THREAD_INITIAL);
    if (requeue_owner->state() == THREAD_DEATH) {
      requeue_owner = nullptr;
    }
  }

  // Wake the specified number of threads and assign a new owner if needed.
  WakeThreadsInternal(wake_count, now, on_thread_wake_hook);

  // If the requeue target currently has an owner, and the owner is changing,
  // start by clearing the owner from the queue.
  if (requeue_target->owner_ && (requeue_target->owner_ != requeue_owner)) {
    requeue_target->AssignOwnerInternal(requeue_owner);
  }

  // If there are still threads left in the wake queue (this), and we were asked to
  // requeue threads, then do so.
  uint32_t requeued = 0;
  if (!this->IsEmpty()) {
    while (requeued < requeue_count) {
      // Consider the thread that the queue considers to be the most important to
      // wake right now.  If there are no threads left in the queue, then we are
      // done.
      Thread* t = Peek(now);
      if (t == nullptr) {
        break;
      }

      // Call the user's requeue hook so that we can decide what to do
      // with this thread.
      using Action = Hook::Action;
      Action action = on_thread_requeue_hook(t);

      // It is illegal to ask for a requeue operation to assign ownership.
      DEBUG_ASSERT(action != Action::SelectAndAssignOwner);

      // If we are supposed to stop, do so now.
      if (action == Action::Stop) {
        break;
      }

      // If attempting to move this thread to the requeue target would create a
      // cycle with the new owner, then clear ownership of the queue instead.
      //
      // TODO(johngro): Change this when we switch to propagating the error
      // instead of simply removing ownership.
      if (CheckForCycle(requeue_target, requeue_owner, t)) {
        requeue_owner = nullptr;
        requeue_target->AssignOwnerInternal(nullptr);
      }

      // Actually move the thread from this to the requeue_target.
      this->collection_.Remove(t);
      t->wait_queue_state().blocking_wait_queue_ = nullptr;
      this->UpdateSchedStateStorageThreadRemoved(*t);
      BeginPropagate(*t, *this, RemoveSingleEdgeOp);

      requeue_target->collection_.Insert(t);
      t->wait_queue_state().blocking_wait_queue_ = requeue_target;
      requeue_target->UpdateSchedStateStorageThreadAdded(*t);
      BeginPropagate(*t, *requeue_target, AddSingleEdgeOp);

      ++requeued;

      // SelectAndKeepGoing is the only legal choice left.
      DEBUG_ASSERT(action == Action::SelectAndKeepGoing);
    }
  }

  // If there is no one waiting in the requeue target, then it is not allowed to
  // have an owner.
  if (requeue_target->IsEmpty()) {
    requeue_owner = nullptr;
  }
  requeue_target->AssignOwnerInternal(requeue_owner);
}

// Explicit instantiation of a variant of the generic BeginPropagate method used in
// wait.cc during thread unblock operations.
template void OwnedWaitQueue::BeginPropagate(Thread&, OwnedWaitQueue&, RemoveSingleEdgeTag);
