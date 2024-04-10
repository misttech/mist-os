// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_SCHED_INCLUDE_LIB_SCHED_RUN_QUEUE_H_
#define ZIRCON_KERNEL_LIB_SCHED_INCLUDE_LIB_SCHED_RUN_QUEUE_H_

#include <zircon/assert.h>

#include <cstdint>
#include <type_traits>
#include <utility>

#include <fbl/intrusive_wavl_tree.h>
#include <fbl/wavl_tree_best_node_observer.h>

#include "thread-base.h"

namespace sched {

// The RunQueue manages the scheduling business logic of its containing threads.
//
// We say a thread is *eligible* at a given time if that time falls within the
// thread's current activation period. At a high level, we try to always
// schedule the currently eligible with the minimal finish time - and it is this
// policy that RunQueue implements.
//
// `Thread` must inherit from `ThreadBase<Thread>`.
template <typename Thread>
class RunQueue {
 private:
  // Forward-declared; defined below.
  struct NodeTraits;
  struct SubtreeMinFinishObserverTraits;

  using Tree =
      fbl::WAVLTree<typename NodeTraits::KeyType, Thread*, NodeTraits, fbl::DefaultObjectTag,
                    NodeTraits, fbl::WAVLTreeBestNodeObserver<SubtreeMinFinishObserverTraits>>;

 public:
  static_assert(std::is_base_of_v<ThreadBase<Thread>, Thread>);

  ~RunQueue() { ready_.clear(); }

  // Threads are iterated through in order of start time.
  using iterator = typename Tree::const_iterator;

  iterator begin() const { return ready_.begin(); }
  iterator end() const { return ready_.end(); }

  // Returns the number of queued, ready threads.
  size_t size() const { return ready_.size(); }

  // Whether no threads are currently queued.
  bool empty() const { return ready_.is_empty(); }

  // Returns the currently scheduled thread, if there is one.
  const Thread* current_thread() const { return current_; }

  // The total duration within the thread's activation period in which the
  // thread is expected to complete its work. This can be regarded as a measure
  // of the expected worst-case runtime.
  Duration CapacityOf(const Thread& thread) const {
    // TODO(https://fxbug.dev/328641440): Account for flexible capacity when
    // introduced.
    return thread.firm_capacity();
  }

  // The remaining time expected for the thread to be scheduled within its
  // current activation period.
  Duration TimesliceRemainingOn(const Thread& thread) const {
    return CapacityOf(thread) - thread.time_slice_used();
  }

  // Whether the thread has completed its work for the current activation
  // period or activation period itself has ended.
  bool IsExpired(const Thread& thread, Time now) const {
    return TimesliceRemainingOn(thread) <= 0 || now >= thread.finish();
  }

  // Queues the provided thread, given the current time (so as to ensure that
  // the thread is or will be active).
  void Queue(Thread& thread, Time now) {
    ZX_DEBUG_ASSERT(!thread.IsQueued());
    if (IsExpired(thread, now)) {
      thread.Reactivate(now);
    }
    ready_.insert(&thread);
  }

  // Dequeues the thread from the run queue (provided the thread was already
  // contained).
  void Dequeue(Thread& thread) {
    ZX_DEBUG_ASSERT(thread.IsQueued());
    ready_.erase(thread);
  }

  struct SelectNextThreadResult {
    Thread* next = nullptr;
    Time preemption_time = Time::Min();
  };

  // Selects the next thread to run and also gives the time at which the
  // preemption timer should fire for the subsequent round of scheduling.
  //
  // The next thread is the one that is active and has the earliest finish time
  // within its current period. In the event of a tie of finish times, the one
  // with the earlier start time is picked - and then in the event of a tie of
  // start times, the thread with the lowest address is expediently picked
  // (which is a small bias that should not persist across runs of the system).
  //
  // If no threads are eligible, nullptr is returned, along with the time at
  // which the next thread should become eligible.
  SelectNextThreadResult SelectNextThread(Time now) {
    // The next eligible might actually be expired (e.g., due to bandwidth
    // oversubscription), in which case it should be reactivated and
    // requeued, and our search should begin again for an eligible thread
    // still in its current period.
    Thread* next = FindNextEligibleThread(now).CopyPointer();
    while (next && now >= next->finish()) {
      Dequeue(*next);
      Queue(*next, now);
      next = FindNextEligibleThread(now).CopyPointer();
    }

    // Ensure `current` is activated before having it - and its otherwise false
    // finish time - factor into the next round of scheduling decisions.
    if (current_ && IsExpired(*current_, now)) {
      current_->Reactivate(now);
    }

    // Try to avoid rebalancing (from tree insertion and deletion) in the case
    // where the next thread is the current one.
    if (current_ && current_->IsActive(now) &&
        (!next || SchedulesBeforeIfActive(*current_, *next))) {
      next = current_;
    } else {
      if (next) {
        Dequeue(*next);
      }
      if (current_) {
        Queue(*current_, now);
      }
      current_ = next;
    }

    Time preemption;
    if (next) {
      Time next_completion = std::min<Time>(now + TimesliceRemainingOn(*next), next->finish());
      ZX_DEBUG_ASSERT(TimesliceRemainingOn(*next) > 0);
      ZX_DEBUG_ASSERT(now < next->finish());

      // Check if there is a thread with an earlier finish that will become
      // eligible before `next` finishes.
      if (auto it = FindNextEligibleThread(next_completion); it && it->finish() < next->finish()) {
        preemption = std::min<Time>(next_completion, it->start());
      } else {
        preemption = next_completion;
      }
    } else {
      // If there is nothing currently eligible, we should preempt next when
      // there is.
      preemption = empty() ? Time::Max() : begin()->start();
    }

    ZX_DEBUG_ASSERT((!next && preemption == Time::Max()) || preemption > now);
    return {next, preemption};
  }

 private:
  using mutable_iterator = typename Tree::iterator;

  // Implements both the WAVLTree key and node traits.
  struct NodeTraits {
    // Start time, along with the address of the thread as a convenient
    // tie-breaker.
    using KeyType = std::pair<Time, uintptr_t>;

    static KeyType GetKey(const Thread& thread) {
      return std::make_pair(thread.start(), reinterpret_cast<uintptr_t>(&thread));
    }

    static bool LessThan(KeyType a, KeyType b) { return a < b; }

    static bool EqualTo(KeyType a, KeyType b) { return a == b; }

    static auto& node_state(Thread& thread) { return thread.run_queue_.node; }
  };

  // Implements with traits for the WAVLTreeBestNodeObserver, which will
  // automatically manage a subtree's minimum finish time on insertion and
  // deletion.
  //
  // The value is used to perform a partition search in O(log n) time, to find
  // the thread with the earliest finish time that also has an eligible start
  // time. See FindNextEligibleThread().
  struct SubtreeMinFinishObserverTraits {
    static Time GetValue(const Thread& thread) { return thread.finish(); }

    static Time GetSubtreeBest(const Thread& thread) {
      return thread.run_queue_.subtree_min_finish;
    }

    static bool Compare(Time a, Time b) { return a < b; }

    static void AssignBest(Thread& thread, Time val) { thread.run_queue_.subtree_min_finish = val; }

    static void ResetBest(Thread& target) {}
  };

  // Provided both threads are active, this gives whether the first should be
  // scheduled before the second, which is a simple lexicographic order on
  // (finish, start, address). (The address is a guaranteed and convenient final
  // tiebreaker which should never amount to a consistent bias.)
  //
  // This comparison is only valid if the given threads are active. It is the
  // caller's responsibility to ensure that this is the case.
  static constexpr bool SchedulesBeforeIfActive(const Thread& a, const Thread& b) {
    return std::make_tuple(a.finish(), a.start(), &a) < std::make_tuple(b.finish(), b.start(), &b);
  }

  // Shorthand for convenience and readability.
  static constexpr Time SubtreeMinFinish(mutable_iterator it) {
    return it->run_queue_.subtree_min_finish;
  }

  // Returns the thread eligible to scheduled at a given time with the minimal
  // finish time.
  mutable_iterator FindNextEligibleThread(Time time) {
    if (ready_.is_empty() || ready_.front().start() > time) {
      return ready_.end();
    }

    // `node` will follow a search path that partitions the tree into eligible
    // tasks, iterating to the subtree of eligible start times and then cleaving
    // to the right. `path` will track the minimum finish time encountered along
    // the search path, while `subtree` will track the subtree with minimum
    // finish time off of the search path. After the search, we will be able to
    // check `path` against `subtree` to determine where the true minimal,
    // eligible finish lies.
    mutable_iterator node = ready_.root();
    mutable_iterator path = ready_.end();
    mutable_iterator subtree = ready_.end();
    while (node) {
      // Iterate to the subtree of eligible start times.
      if (node->start() > time) {
        node = node.left();
        continue;
      }

      // Earlier finish found on path: update `path`.
      if (!path || path->finish() > node->finish()) {
        path = node;
      }

      // Earlier finish found off path: update `subtree`.
      {
        auto left = node.left();
        if (!subtree || (left && SubtreeMinFinish(subtree) > SubtreeMinFinish(left))) {
          subtree = left;
        }
      }

      node = node.right();
    }

    // Check if the minimum eligible finish was found along the search path. If
    // there is an identical finish time in the subtree, respect the documented
    // tiebreaker policy and go with the subtree's thread.
    if (!subtree || SubtreeMinFinish(subtree) > path->finish()) {
      return path;
    }

    // Else, the minimum eligible finish must exist in `subtree`.
    for (node = subtree; node->finish() != SubtreeMinFinish(subtree);) {
      if (auto left = node.left(); left && SubtreeMinFinish(node) == SubtreeMinFinish(left)) {
        node = left;
      } else {
        node = node.right();
      }
    }
    return node;
  }

  // The thread currently selected to be run.
  Thread* current_ = nullptr;

  // The tree of threads ready to be run.
  Tree ready_;
};

}  // namespace sched

#endif  // ZIRCON_KERNEL_LIB_SCHED_INCLUDE_LIB_SCHED_RUN_QUEUE_H_
