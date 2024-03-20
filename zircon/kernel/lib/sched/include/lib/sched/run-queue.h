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

  ~RunQueue() { tree_.clear(); }

  // Threads are iterated through in order of start time.
  using iterator = typename Tree::const_iterator;

  iterator begin() const { return tree_.begin(); }
  iterator end() const { return tree_.end(); }

  // Returns the number of queued threads.
  size_t size() const { return tree_.size(); }

  // Whether no threads are currently queued.
  bool empty() const { return tree_.is_empty(); }

  // Queues the provided thread, given the current time (so as to ensure that
  // the thread is or will be active).
  void Queue(Thread& thread, Time now) {
    thread.ReactivateIfExpired(now);
    tree_.insert(&thread);
  }

  // Dequeues the thread from the run queue (provided the thread was already
  // contained).
  void Dequeue(Thread& thread) { tree_.erase(thread); }

  struct EvaluateNextThreadResult {
    Thread* next = nullptr;
    Time preemption_time = Time::Min();
  };

  // Given the most recently executed thread, evaluates the next thread that
  // should be scheduled and the time at which the preemption timer should fire
  // for the subsequent round of scheduling.
  EvaluateNextThreadResult EvaluateNextThread(Thread& current, Time now) {
    // Ensure `current` is activated before having it - and its otherwise false
    // finish time - factor into the next round of scheduling decisions.
    current.ReactivateIfExpired(now);

    Thread* next = nullptr;
    while (!next) {
      // Try to avoid rebalancing (from tree insertion and deletion) in the case
      // where the next thread is the current one.
      //
      // As a tie-breaker when the next eligible has the same finish time as the
      // current, ensure a wider variety of work being done by letting the other
      // one have its turn.
      if (auto it = FindNextEligibleThread(now); it && it->finish() <= current.finish()) {
        // The next eligible might actually be expired (e.g., due to bandwidth
        // oversubscription), in which case it should be reactivated and
        // requeued, and our search should begin again for an eligible thread
        // still in its current period.
        next = it.CopyPointer();
        Dequeue(*next);
        Thread& requeued = now < next->finish() ? current : *std::exchange(next, nullptr);
        Queue(requeued, now);
      } else {
        next = &current;
      }
    }

    ZX_DEBUG_ASSERT(next->time_slice_remaining() > 0);
    ZX_DEBUG_ASSERT(now < next->finish());
    Time next_completion = std::min<Time>(now + next->time_slice_remaining(), next->finish());

    // Check if there is a thread with an earlier finish that will become
    // eligible before `next` finishes.
    Time preemption;
    if (auto it = FindNextEligibleThread(next_completion); it && it->finish() < next->finish()) {
      preemption = std::min<Time>(next_completion, it->start());
    } else {
      preemption = next_completion;
    }

    ZX_DEBUG_ASSERT(preemption > now);
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

  // Shorthand for convenience and readability.
  static constexpr Time SubtreeMinFinish(mutable_iterator it) {
    return it->run_queue_.subtree_min_finish;
  }

  // Returns the thread eligible to scheduled at a given time with the minimal
  // finish time.
  mutable_iterator FindNextEligibleThread(Time time) {
    if (tree_.is_empty() || tree_.front().start() > time) {
      return tree_.end();
    }

    // `node` will follow a search path that partitions the tree into eligible
    // tasks, iterating to the subtree of eligible start times and then cleaving
    // to the right. `path` will track the minimum finish time encountered along
    // the search path, while `subtree` will track the subtree with minimum
    // finish time off of the search path. After the search, we will be able to
    // check `path` against `subtree` to determine where the true minimal,
    // eligible finish lies.
    mutable_iterator node = tree_.root();
    mutable_iterator path = tree_.end();
    mutable_iterator subtree = tree_.end();
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

    // Check if the minimum eligible finish was found along the search path.
    if (!subtree || SubtreeMinFinish(subtree) >= path->finish()) {
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

  Tree tree_;
};

}  // namespace sched

#endif  // ZIRCON_KERNEL_LIB_SCHED_INCLUDE_LIB_SCHED_RUN_QUEUE_H_
