// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/kconcurrent/chainlock_transaction.h>
#include <lib/unittest/unittest.h>

#include <kernel/auto_preempt_disabler.h>
#include <kernel/event.h>
#include <kernel/owned_wait_queue.h>
#include <kernel/thread.h>
#include <ktl/unique_ptr.h>

static constexpr uint32_t Invalid = ktl::numeric_limits<uint32_t>::max();

struct OwnedWaitQueueTopologyTests {
  class TestQueue;
  class TestThread {
   public:
    constexpr TestThread() = default;
    ~TestThread() { Shutdown(); }

    static SchedDuration DeadlineForIndex(size_t index) {
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      constexpr SchedDuration kBaseDuration{ZX_MSEC(10)};
      constexpr SchedDuration kDurationInc{ZX_MSEC(1)};
#else
      constexpr SchedDuration kBaseDuration{ZX_USEC(500)};
      constexpr SchedDuration kDurationInc{ZX_USEC(10)};
#endif
      return kBaseDuration + (index * kDurationInc);
    }

    bool Init(const SchedulerState::BaseProfile* base_profile = nullptr) {
      BEGIN_TEST;

      ASSERT_NULL(thread_);
      thread_ = Thread::Create(
          "owq_test_thread",
          [](void* arg) -> int { return reinterpret_cast<TestThread*>(arg)->Main(); }, this,
          DEFAULT_PRIORITY);
      ASSERT_NONNULL(thread_);
      if (base_profile != nullptr) {
        thread_->SetBaseProfile(*base_profile);
      }
      thread_->Resume();

      END_TEST;
    }

    void Shutdown() {
      exit_now_.store(true);
      if (thread_ != nullptr) {
        int unused_retcode;
        thread_->Kill();
        thread_->Join(&unused_retcode, ZX_TIME_INFINITE);
        thread_ = nullptr;
      }
    }

    bool DoBlock(TestQueue& queue, TestThread* new_owner);
    void SetStartTimeGate(zx_instant_mono_t time) { start_time_gate_ = time; }
    Thread* thread() { return thread_; }
    const OwnedWaitQueue* target_queue() const { return target_queue_; }
    const OwnedWaitQueue* blocking_queue() const TA_EXCL(chainlock_transaction_token) {
      ASSERT(thread_ != nullptr);
      SingleChainLockGuard guard{
          IrqSaveOption, thread_->get_lock(),
          CLT_TAG("OwnedWaitQueueTopologyTests::TestThread::blocking_queue")};
      return OwnedWaitQueue::DowncastToOwq(thread_->wait_queue_state().blocking_wait_queue());
    }

    zx_instant_mono_t start_time() const {
      ASSERT(thread_ != nullptr);
      SingleChainLockGuard guard{IrqSaveOption, thread_->get_lock(),
                                 CLT_TAG("OwnedWaitQueueTopologyTests::TestThread::start_time")};
      return thread_->scheduler_state().start_time().raw_value();
    }

    zx_instant_mono_t finish_time() const {
      ASSERT(thread_ != nullptr);
      SingleChainLockGuard guard{IrqSaveOption, thread_->get_lock(),
                                 CLT_TAG("OwnedWaitQueueTopologyTests::TestThread::start_time")};
      return thread_->scheduler_state().finish_time().raw_value();
    }

   private:
    int Main();

    static inline WaitQueue final_exit_queue_;

    ktl::atomic<bool> exit_now_{false};
    ktl::atomic<bool> do_baao_{false};
    ktl::optional<zx_instant_mono_t> start_time_gate_{ktl::nullopt};
    OwnedWaitQueue* target_queue_{nullptr};
    Thread* target_owner_{nullptr};
    Thread* thread_{nullptr};
  };

  class TestQueue {
   public:
    constexpr TestQueue() = default;
    ~TestQueue() = default;

    bool Init() {
      BEGIN_TEST;

      ASSERT_NULL(wait_queue_);
      fbl::AllocChecker ac;
      wait_queue_.reset(new (&ac) OwnedWaitQueue{});
      ASSERT_TRUE(ac.check());

      END_TEST;
    }

    OwnedWaitQueue* wait_queue() { return wait_queue_.get(); }
    Thread* owner() {
      ASSERT(wait_queue_ != nullptr);
      SingleChainLockGuard guard{IrqSaveOption, wait_queue_->get_lock(),
                                 CLT_TAG("OwnedWaitQueueTopologyTests::TestQueue::owner")};
      return wait_queue_->owner();
    }

   private:
    ktl::unique_ptr<OwnedWaitQueue> wait_queue_;
  };

  struct Action {
    uint32_t thread_index;         // Index of the blocking thread.
    uint32_t queue_index;          // Index of the queue which the thread blocks behind.
    uint32_t owning_thread_index;  // Index of the thread declared to be the owner.
    bool owner_expected;           // True if we expect the owner to be accepted, false if
                                   // we expect the owner to be rejected (and end up with
                                   // no owner after the action.
  };

  struct RequeueAction {
    uint32_t wake_count;
    uint32_t requeue_count;
    uint32_t requeue_owner_index;
    bool assign_woken;
  };

  template <size_t ThreadCount>
  static void ShutdownThreads(ktl::array<TestThread, ThreadCount>& threads) {
    for (TestThread& t : threads) {
      t.Shutdown();
    }
  }

  template <size_t ThreadCount, size_t QueueCount, size_t ActionCount>
  static bool RunBAAOTest(ktl::array<TestThread, ThreadCount>& threads,
                          ktl::array<TestQueue, QueueCount>& queues,
                          const ktl::array<Action, ActionCount>& actions,
                          bool setup_for_requeue_test = false) {
    BEGIN_TEST;

    // Initialize all of the threads and queues.  We cannot proceed if any of this
    // fails.
    for (size_t i = 0; i < threads.size(); ++i) {
      if (!setup_for_requeue_test) {
        ASSERT_TRUE(threads[i].Init());
      } else {
        const SchedDuration deadline = TestThread::DeadlineForIndex(i);
        const SchedDuration capacity = deadline / 2;
        const SchedDeadlineParams params{capacity, deadline};
        const SchedulerState::BaseProfile profile{params};
        ASSERT_TRUE(threads[i].Init(&profile));
      }
    }

    for (TestQueue& queue : queues) {
      ASSERT_TRUE(queue.Init());
    }

    // Perform each of the actions, blocking a given thread behind a given wait
    // queue and optionally declaring an owner as we do.  Afterwards, verify that
    // each of the threads is blocked behind the queue we expect, and that the
    // owner is who we expect.
#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
    TestThread* previous_thread{nullptr};
#endif
    for (const Action& action : actions) {
      // Make sue the action indexes are valid.
      ASSERT_LT(action.thread_index, threads.size());
      ASSERT_LT(action.queue_index, queues.size());

      TestThread& blocking_thread = threads[action.thread_index];
      TestQueue& target_queue = queues[action.queue_index];
      TestThread* new_owner{nullptr};
      if (action.owning_thread_index != Invalid) {
        ASSERT_LT(action.owning_thread_index, threads.size());
        new_owner = &threads[action.owning_thread_index];
      }

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      // Make certain that when we are setting up for a re-queue test that we
      // carefully control the order that the threads will be selected from the
      // wait queue when it is time to perform the actual wake-and-requeue
      // operation.
      //
      // Note; this is not normally something that tests can depend on.  In
      // theory, the priority order of a wait queue is officially "not your
      // business" and not formally specified.  When it comes to the specific
      // case of in-kernel unit tests and keeping them deterministic, it is
      // probably OK to be aware of the specifics of how things are ordered,
      // even though this means that when the sorting invariant for the wait
      // queue changes (not a common thing at all), this test will need to be
      // updated as well.
      //
      // At any rate, currently the wake order of threads in a wait queue is
      // controlled completely by their finish time.  Threads with earlier
      // finish times get woken before threads with later finish times.  The
      // finish time of a deadline thread is its start time + its period.  We
      // want the threads to wake in the order that they were setup (T0 wakes
      // first, then T1, then T2 and so on).
      //
      // We have already made certain that the periods of these threads are
      // strictly monotonically increasing, so now we just need to ensure that
      // the start times are monotonically increasing and we should be good to
      // go.
      //
      // To accomplish this, each thread has an optional "start time gate" which
      // can be assigned.  The first thread can start whenever it wants to, but
      // the thread T(x) needs to make sure that its start time is >= the start
      // time of T(x-1).
      //
      // So, we set T(x).start_time_gate = T(x - 1).start_time for x > 0.  These
      // threads, when released from their initial spin phase of the test, will
      // make certain that their start time is >= their start time gate (when
      // they have one) by sleeping until their finish time until they hit the
      // point that the condition is satisfied.
      //
      if (setup_for_requeue_test && (previous_thread != nullptr)) {
        blocking_thread.SetStartTimeGate(previous_thread->start_time());
      }
      previous_thread = &blocking_thread;
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED

      // Then perform the block operation.
      ASSERT_TRUE(blocking_thread.DoBlock(target_queue, new_owner));

      // Now validate everything.
      Thread* expected_owner =
          action.owner_expected && (new_owner != nullptr) ? new_owner->thread() : nullptr;
      EXPECT_EQ(expected_owner, target_queue.owner());
      for (const TestThread& t : threads) {
        EXPECT_EQ(t.target_queue(), t.blocking_queue());
      }
    }

    if (setup_for_requeue_test) {
#if !EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
      // Sleep for longer than the longest relative deadline we assigned to our
      // threads.  This will ensure that none of the currently blocked threads
      // have an active deadline (all of their deadlines will have expired).
      // Since the relative deadlines of the threads were assigned in increasing
      // order relative to their index in the test thread array, this means that
      // when it comes time to choose a thread to wake up from the blocked
      // queue, we should always end up choosing the thread in the queue with
      // the lowest index in the test thread array.
      Thread::Current::SleepRelative(TestThread::DeadlineForIndex(threads.size()).raw_value());
#endif  // !EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
    } else {
      ShutdownThreads(threads);
    }

    END_TEST;
  }

  template <size_t ThreadCount, size_t QueueCount, size_t ActionCount>
  static bool RunRequeueTest(ktl::array<TestThread, ThreadCount>& threads,
                             ktl::array<TestQueue, QueueCount>& queues,
                             const ktl::array<Action, ActionCount>& setup_actions,
                             const RequeueAction& requeue_action,
                             const ktl::array<uint32_t, ThreadCount>& expected_blocking_queues,
                             const ktl::array<uint32_t, QueueCount>& expected_owners) {
    BEGIN_TEST;

    auto GetThread = [&](uint32_t index) -> Thread* {
      if (index == Invalid) {
        return nullptr;
      }
      DEBUG_ASSERT(index < threads.size());
      return threads[index].thread();
    };

    auto GetQueue = [&](uint32_t index) -> OwnedWaitQueue* {
      if (index == Invalid) {
        return nullptr;
      }
      DEBUG_ASSERT(index < queues.size());
      return queues[index].wait_queue();
    };

    static_assert(QueueCount >= 2, "Requeue tests must always involve at least two queues");

    // Setup the pre-requeue state.
    ASSERT_TRUE(RunBAAOTest(threads, queues, setup_actions, true));

    // Now perform the requeue action
    Thread* const new_requeue_owner = GetThread(requeue_action.requeue_owner_index);
    OwnedWaitQueue::IWakeRequeueHook& hooks = OwnedWaitQueue::default_wake_hooks();
    OwnedWaitQueue& src_queue = *queues[0].wait_queue();
    OwnedWaitQueue& dst_queue = *queues[1].wait_queue();
    src_queue.WakeAndRequeue(dst_queue, new_requeue_owner, requeue_action.wake_count,
                             requeue_action.requeue_count, hooks, hooks,
                             requeue_action.assign_woken ? OwnedWaitQueue::WakeOption::AssignOwner
                                                         : OwnedWaitQueue::WakeOption::None);

    // Now verify that all of the threads are blocked (or not) in the proper
    // queues, and that all queues are owned by the expected threads (or no
    // thread).
    for (size_t thread_index = 0; thread_index < expected_blocking_queues.size(); ++thread_index) {
      const uint32_t queue_index = expected_blocking_queues[thread_index];
      OwnedWaitQueue* const expected_queue = GetQueue(queue_index);
      EXPECT_EQ(expected_queue, threads[thread_index].blocking_queue());
    }

    for (size_t queue_index = 0; queue_index < expected_owners.size(); ++queue_index) {
      const uint32_t thread_index = expected_owners[queue_index];
      Thread* const expected = GetThread(thread_index);
      EXPECT_EQ(expected, queues[queue_index].owner());
    }

    // Cleanup and we are done.
    ShutdownThreads(threads);
    END_TEST;
  }
};

int OwnedWaitQueueTopologyTests::TestThread::Main() {
  while (!do_baao_.load()) {
    Thread::Current::SleepRelative(ZX_USEC(100));
    if (exit_now_.load()) {
      return -1;
    }
  }

#if EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED
  // If we have a "start time gate", then we need make sure that our start time
  // >= to our gate time in order to ensure a deterministic wake order when
  // running requeue tests.  This is a pretty simple operation; while our start
  // time is not large enough, we can just sleep until our finish time (getting
  // a new start time in the process).
  //
  // Note that we know that it is safe to observe the start_time_gate value
  // (from a memory order perspective) as it was stored before the CST store to
  // do_baao_ performed by the test setup thread.
  if (start_time_gate_.has_value()) {
    while (start_time() < start_time_gate_.value()) {
      Thread::Current::Sleep(finish_time());
    }
  }
#endif  // EXPERIMENTAL_UNIFIED_SCHEDULER_ENABLED

  {
    AnnotatedAutoPreemptDisabler aapd;
    ASSERT(target_queue_ != nullptr);
    target_queue_->BlockAndAssignOwner(Deadline::infinite(), target_owner_,
                                       ResourceOwnership::Normal, Interruptible::Yes);
  }

  if (exit_now_.load()) {
    return -1;
  }

  const auto do_transaction =
      [&]() TA_REQ(chainlock_transaction_token) -> ChainLockTransaction::Result<> {
    Thread* const current_thread = Thread::Current::Get();

    while (!AcquireBothOrBackoff(final_exit_queue_.get_lock(), current_thread->get_lock())) {
      return ChainLockTransaction::Action::Backoff;
    }

    ChainLockTransaction::Finalize();
    final_exit_queue_.Block(current_thread, Deadline::infinite(), Interruptible::Yes);
    current_thread->get_lock().Release();
    return ChainLockTransaction::Done;
  };
  ChainLockTransaction::UntilDone(
      IrqSaveOption, CLT_TAG("OwnedWaitQueueTopologyTests::TestThread::Main (final exit block)"),
      do_transaction);

  return 0;
}

bool OwnedWaitQueueTopologyTests::TestThread::DoBlock(TestQueue& queue, TestThread* new_owner) {
  BEGIN_TEST;

  ASSERT_FALSE(do_baao_.load());
  ASSERT_NULL(target_queue_);
  ASSERT_NULL(target_owner_);
  ASSERT_NONNULL(queue.wait_queue());
  if (new_owner) {
    ASSERT_NONNULL(new_owner->thread());
    target_owner_ = new_owner->thread();
  }
  target_queue_ = queue.wait_queue();
  do_baao_.store(true);

  while (true) {
    {
      SingleChainLockGuard guard{IrqSaveOption, thread_->get_lock(),
                                 CLT_TAG("OwnedWaitQueueTopologyTests::TestThread::DoBlock")};
      if (thread_->state() == THREAD_BLOCKED) {
        break;
      }
    }
    Thread::Current::SleepRelative(ZX_USEC(100));
  }

  END_TEST;
}

namespace {

using TestThread = OwnedWaitQueueTopologyTests::TestThread;
using TestQueue = OwnedWaitQueueTopologyTests::TestQueue;
using Action = OwnedWaitQueueTopologyTests::Action;
using RequeueAction = OwnedWaitQueueTopologyTests::RequeueAction;

bool owq_topology_test_simple() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // A basic smoke test.  Simply generate
  //
  // +----+     +----+
  // | T0 | --> | Q0 |
  // +----+     +----+
  //

  ktl::array<TestQueue, 1> queues;
  ktl::array<TestThread, 1> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_simple_owner_change() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Change owners, moving from this
  //
  // +----+     +----+     +----+
  // | T0 | --> | Q0 | --> | T2 |
  // +----+     +----+     +----+
  //
  // to this:
  //
  // +----+     +----+     +----+
  // | T0 | --> | Q0 | --> | T3 |
  // +----+  ^  +----+     +----+
  //         |
  // +----+  |  +----+
  // | T1 | -+  | Q0 |
  // +----+     +----+

  ktl::array<TestQueue, 1> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 2, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_deny_self_own() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to do this
  //
  //  +----------------------+
  //  |                      |
  //  |  +----+     +----+   |
  //  +->| T0 | --> | Q0 | --+
  //     +----+     +----+
  //
  // Which should be denied, ending up with this.
  //
  //     +----+     +----+
  //     | T0 | --> | Q0 |
  //     +----+     +----+
  //

  ktl::array<TestQueue, 1> queues;
  ktl::array<TestThread, 1> threads;
  constexpr ktl::array actions{
      Action{
          .thread_index = 0, .queue_index = 0, .owning_thread_index = 0, .owner_expected = false},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_owner_becomes_blocked_no_new_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 |
  //     +----+     +----+     +----+
  //
  // To this, by having T1 block, and declare no new owner.
  //
  //     +----+     +----+
  //     | T0 | --> | Q0 |
  //     +----+  ^  +----+
  //             |
  //     +----+  |
  //     | T1 | -+
  //     +----+
  //

  ktl::array<TestQueue, 1> queues;
  ktl::array<TestThread, 2> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_owner_becomes_blocked_yes_new_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 |
  //     +----+     +----+     +----+
  //
  // To this, by having T1 block, and declare T2 as the new owner.
  //
  //     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T2 |
  //     +----+  ^  +----+     +----+
  //             |
  //     +----+  |
  //     | T1 | -+
  //     +----+
  //
  ktl::array<TestQueue, 1> queues;
  ktl::array<TestThread, 3> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 2, .owner_expected = true},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_owner_becomes_blocked_deny_blocked_new_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 |
  //     +----+     +----+     +----+
  //
  // To this, by having T1 block, and declare T0 as the owner.
  //
  //  +----------------------+
  //  |                      |
  //  |  +----+     +----+   |
  //  +->| T0 | --> | Q0 | --+
  //     +----+  ^  +----+
  //             |
  //     +----+  |
  //     | T1 | -+
  //     +----+
  //
  // Because of the cycle, this should be denied, resulting in this:
  //
  //     +----+     +----+
  //     | T0 | --> | Q0 |
  //     +----+  ^  +----+
  //             |
  //     +----+  |
  //     | T1 | -+
  //     +----+
  //

  ktl::array<TestQueue, 1> queues;
  ktl::array<TestThread, 2> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
      Action{
          .thread_index = 1, .queue_index = 0, .owning_thread_index = 0, .owner_expected = false},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_blocked_owners() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 | --> | Q1 |
  //     +----+     +----+     +----+     +----+
  //
  //                           +----+     +----+
  //                           | T2 | --> | Q2 |
  //                           +----+     +----+
  //
  // To this, by having T3 block, and declare T2 as the owner.
  //
  //                           +----+     +----+
  //                           | T1 | --> | Q1 |
  //                           +----+     +----+
  //
  //     +----+     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T2 | --> | Q2 |
  //     +----+  ^  +----+     +----+     +----+
  //             |
  //     +----+  |
  //     | T3 | -+
  //     +----+
  //

  ktl::array<TestQueue, 3> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 1,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 2,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 0, .owning_thread_index = 2, .owner_expected = true},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_blocking_thread_downstream_of_owner_no_owner_change() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+     +----+     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 | --> | Q1 | --> | T2 |
  //     +----+     +----+     +----+     +----+     +----+
  //
  //
  // To this, by having T2 block in Q0, declaring T1 to still be the owner.
  //
  //     +----+     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 | --> | Q1 |
  //     +----+  ^  +----+     +----+     +----+
  //             |                          |
  //     +----+  |                          |
  //     | T2 | -+                          |
  //     +----+                             |
  //       ^                                |
  //       |                                |
  //       +--------------------------------+
  //
  // This should fail because of the cycle, and result in this instead.
  //
  //                          +----+     +----+
  //                          | T0 | --> | Q0 |
  //                          +----+  ^  +----+
  //                                  |
  //    +----+     +----+     +----+  |
  //    | T1 | --> | Q1 | --> | T2 | -+
  //    +----+     +----+     +----+
  //

  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 3> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 1, .owning_thread_index = 2, .owner_expected = true},
      Action{
          .thread_index = 2, .queue_index = 0, .owning_thread_index = 1, .owner_expected = false},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_blocking_thread_downstream_of_owner_yes_owner_change() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q1 | --> | T3 |
  //     +----+     +----+     +----+
  //
  // To this, by having T3 block in Q0, declaring T2 to be the new owner.
  //
  //     +----+
  //     | T1 |
  //     +----+
  //
  //     +----+     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T2 | --> | Q1 |
  //     +----+  ^  +----+     +----+     +----+
  //             |                          |
  //     +----+  |                          |
  //     | T3 | -+                          |
  //     +----+                             |
  //       ^                                |
  //       |                                |
  //       +--------------------------------+
  //
  // This should fail because of the cycle, and result in this instead.
  //     +----+
  //     | T1 |
  //     +----+
  //
  //     +----+     +----+
  //     | T0 | --> | Q0 |
  //     +----+  ^  +----+
  //             |
  //     +----+  |
  //     | T3 | -+
  //     +----+
  //
  //     +----+     +----+
  //     | T2 | --> | Q1 |
  //     +----+     +----+
  //

  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 1, .owning_thread_index = 3, .owner_expected = true},
      Action{
          .thread_index = 3, .queue_index = 0, .owning_thread_index = 2, .owner_expected = false},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_intersecting_owner_chains() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+     +----+     +----+     +----+     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 | --> | Q1 | --> | T2 | --> | Q2 | --> | T3 |
  //     +----+     +----+     +----+     +----+     +----+  ^  +----+     +----+
  //                                                         |
  //                                                 +----+  |
  //                                                 | T4 | -+
  //                                                 +----+
  //
  // To this, by having T5 block in Q0, declaring T4 to be the new owner.
  //
  //     +----+     +----+     +----+
  //     | T1 | --> | Q1 | --> | T2 | -+
  //     +----+     +----+     +----+  |
  //                                   |
  //     +----+     +----+     +----+  V  +----+     +----+
  //     | T0 | --> | Q0 | --> | T4 | --> | Q2 | --> | T3 |
  //     +----+  ^  +----+     +----+     +----+     +----+
  //             |
  //     +----+  |
  //     | T5 | -+
  //     +----+
  //
  ktl::array<TestQueue, 3> queues;
  ktl::array<TestThread, 6> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 1, .owning_thread_index = 2, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 2, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 4, .queue_index = 2, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 5, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_intersecting_owner_chains_old_owner_target_of_new_owner_chain() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //                           +----+     +----+     +----+
  //                           | T0 | --> | Q0 | --> | T3 |
  //                           +----+     +----+  ^  +----+
  //                                              |
  //     +----+     +----+     +----+     +----+  |
  //     | T1 | --> | Q1 | --> | T2 | --> | Q2 | -+
  //     +----+     +----+     +----+     +----+
  //
  // To this, by having T4 block in Q0, declaring T1 to be the new owner.
  //
  //     +----+
  //     | T4 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+     +----+     +----+     +----+     +----+     +----+
  //     | T0 | --> | Q0 | --> | T1 | --> | Q1 | --> | T2 | --> | Q2 | --> | T3 |
  //     +----+     +----+     +----+     +----+     +----+     +----+     +----+
  //
  //

  ktl::array<TestQueue, 3> queues;
  ktl::array<TestThread, 5> threads;
  constexpr ktl::array actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 1, .owning_thread_index = 2, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 2, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 4, .queue_index = 0, .owning_thread_index = 1, .owner_expected = true},
  };

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunBAAOTest(threads, queues, actions));

  END_TEST;
}

bool owq_topology_test_basic_requeue() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+          +----+
  //     | T1 | --> | Q0 |          | Q1 |
  //     +----+  ^  +----+          +----+
  //             |
  //     +----+  |
  //     | T2 | -+
  //     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1
  //
  //     +----+
  //     | T0 |
  //     +----+
  //
  //     +----+     +----+
  //     | T2 | --> | Q0 |
  //     +----+     +----+
  //
  //     +----+     +----+
  //     | T1 | --> | Q1 |
  //     +----+     +----+
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 3> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = false};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u};
  constexpr ktl::array expected_owners{Invalid, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_assign_woken_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+          +----+
  //     | T1 | --> | Q0 |          | Q1 |
  //     +----+  ^  +----+          +----+
  //             |
  //     +----+  |
  //     | T2 | -+
  //     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner in the process.
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+
  //     | T1 | --> | Q1 |
  //     +----+     +----+
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 3> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_assign_both_owners() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+          +----+
  //     | T1 | --> | Q0 |          | Q1 |
  //     +----+  ^  +----+          +----+
  //             |
  //     +----+  |                  +----+
  //     | T2 | -+                  | T3 |
  //     +----+                     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and T3 as the Q1 owner in the process.
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+     +----+
  //     | T1 | --> | Q1 | --> | T3 |
  //     +----+     +----+     +----+
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 3, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, Invalid};
  constexpr ktl::array expected_owners{0u, 3u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_owner_in_wake_queue() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+          +----+
  //     | T1 | --> | Q0 |          | Q1 |
  //     +----+  ^  +----+          +----+
  //             |
  //     +----+  |
  //     | T2 | -+
  //     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and T2 as the new requeue owner.
  //
  //     +----+     +----+     +----+     +----+     +----+
  //     | T1 | --> | Q1 | --> | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+     +----+     +----+
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 3> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 2, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u};
  constexpr ktl::array expected_owners{0u, 2u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_owner_is_new_wake_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+          +----+
  //     | T1 | --> | Q0 |          | Q1 |
  //     +----+  ^  +----+          +----+
  //             |
  //     +----+  |
  //     | T2 | -+
  //     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and T0 as the new requeue owner.
  //
  //     +----+     +----+
  //     | T1 | --> | Q1 | -+
  //     +----+     +----+  |
  //                        |
  //     +----+     +----+  V  +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 3> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 0, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u};
  constexpr ktl::array expected_owners{0u, 0u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_owner_is_in_requeue_target() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+          +----+
  //     | T1 | --> | Q0 |          | Q1 |
  //     +----+  ^  +----+          +----+
  //             |
  //     +----+  |
  //     | T2 | -+
  //     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and T1 as the new requeue owner.
  //
  //          +----+     +----+
  //      +-> | T1 | --> | Q1 | -+
  //      |   +----+     +----+  |
  //      |                      |
  //      +----------------------+
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  // This will fail because of the cycle, and should result in:
  //
  //     +----+     +----+
  //     | T1 | --> | Q1 |
  //     +----+     +----+
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 3> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 1, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_wake_owner_becomes_requeue_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+     +----+          +----+
  //     | T1 | --> | Q0 | --> | T3 |          | Q1 |
  //     +----+  ^  +----+     +----+          +----+
  //             |
  //     +----+  |
  //     | T2 | -+
  //     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and T3 as the new requeue owner.
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+     +----+
  //     | T1 | --> | Q1 | --> | T3 |
  //     +----+     +----+     +----+
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 3, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, Invalid};
  constexpr ktl::array expected_owners{0u, 3u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_wake_and_requeue_have_same_owner_keep_rq_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+     +----+
  //     | T1 | --> | Q0 | --> | T4 |
  //     +----+  ^  +----+  ^  +----+
  //             |          |
  //     +----+  |          |
  //     | T2 | -+          |
  //     +----+             |
  //                        |
  //     +----+     +----+  |
  //     | T3 | --> | Q1 | -+
  //     +----+     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and keep T4 as the requeue owner.
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+     +----+
  //     | T1 | --> | Q1 | --> | T4 |
  //     +----+  ^  +----+     +----+
  //             |
  //     +----+  |
  //     | T3 | -+
  //     +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 5> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 4, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 4, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u, Invalid};
  constexpr ktl::array expected_owners{0u, 4u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_wake_and_requeue_have_same_owner_lose_rq_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+     +----+
  //     | T1 | --> | Q0 | --> | T4 |
  //     +----+  ^  +----+  ^  +----+
  //             |          |
  //     +----+  |          |
  //     | T2 | -+          |
  //     +----+             |
  //                        |
  //     +----+     +----+  |
  //     | T3 | --> | Q1 | -+
  //     +----+     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and no owner for Q1.
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+
  //     | T1 | --> | Q1 |
  //     +----+  ^  +----+
  //             |
  //     +----+  |
  //     | T3 | -+
  //     +----+
  //
  //     +----+
  //     | T4 |
  //     +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 5> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 4, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u, Invalid};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_op_intersecting_owner_chains_no_rq_owner_change() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+     +----+     +----+     +----+     +----+
  //     | T1 | --> | Q0 | --> | T4 | --> | Q2 | --> | T5 | --> | Q3 |
  //     +----+  ^  +----+     +----+     +----+  ^  +----+     +----+
  //             |                                |
  //     +----+  |                                |
  //     | T2 | -+                                |
  //     +----+                                   |
  //                                              |
  //                           +----+     +----+  |
  //                           | T3 | --> | Q1 | -+
  //                           +----+     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and keeping T5 as the owner for Q1.
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+     +----+     +----+
  //     | T4 | --> | Q2 | --> | T5 | --> | Q3 |
  //     +----+     +----+  ^  +----+     +----+
  //                        |
  //     +----+     +----+  |
  //     | T3 | --> | Q1 | -+
  //     +----+  ^  +----+
  //             |
  //     +----+  |
  //     | T1 | -+
  //     +----+
  //
  //
  ktl::array<TestQueue, 4> queues;
  ktl::array<TestThread, 6> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 5, .owner_expected = true},
      Action{.thread_index = 4, .queue_index = 2, .owning_thread_index = 5, .owner_expected = true},
      Action{.thread_index = 5,
             .queue_index = 3,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 5, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u, 2u, 3u};
  constexpr ktl::array expected_owners{0u, 5u, 5u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_op_intersecting_owner_chains_change_rqo_to_none() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+     +----+     +----+     +----+     +----+
  //     | T1 | --> | Q0 | --> | T4 | --> | Q2 | --> | T5 | --> | Q3 |
  //     +----+  ^  +----+     +----+     +----+  ^  +----+     +----+
  //             |                                |
  //     +----+  |                                |
  //     | T2 | -+                                |
  //     +----+                                   |
  //                                              |
  //                           +----+     +----+  |
  //                           | T3 | --> | Q1 | -+
  //                           +----+     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing no-owner for Q1.
  //
  //     +----+     +----+     +----+
  //     | T2 | --> | Q0 | --> | T0 |
  //     +----+     +----+     +----+
  //
  //     +----+     +----+     +----+     +----+
  //     | T4 | --> | Q2 | --> | T5 | --> | Q3 |
  //     +----+     +----+     +----+     +----+
  //
  //     +----+     +----+
  //     | T3 | --> | Q1 |
  //     +----+  ^  +----+
  //             |
  //     +----+  |
  //     | T1 | -+
  //     +----+
  //
  //
  ktl::array<TestQueue, 4> queues;
  ktl::array<TestThread, 6> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 5, .owner_expected = true},
      Action{.thread_index = 4, .queue_index = 2, .owning_thread_index = 5, .owner_expected = true},
      Action{.thread_index = 5,
             .queue_index = 3,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u, 2u, 3u};
  constexpr ktl::array expected_owners{0u, Invalid, 5u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_op_intersecting_owner_chains_change_rqo_to_wq_thread() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //     +----+
  //     | T0 | -+
  //     +----+  |
  //             |
  //     +----+  V  +----+     +----+     +----+     +----+     +----+
  //     | T1 | --> | Q0 | --> | T4 | --> | Q2 | --> | T5 | --> | Q3 |
  //     +----+  ^  +----+     +----+     +----+  ^  +----+     +----+
  //             |                                |
  //     +----+  |                                |
  //     | T2 | -+                                |
  //     +----+                                   |
  //                                              |
  //                           +----+     +----+  |
  //                           | T3 | --> | Q1 | -+
  //                           +----+     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing T2 as the new owner of Q1.
  //
  //     +----+     +----+     +----+     +----+     +----+
  //     | T3 | --> | Q1 | --> | T2 | --> | Q0 | --> | T0 |
  //     +----+  ^  +----+     +----+     +----+     +----+
  //             |
  //     +----+  |
  //     | T1 | -+
  //     +----+
  //
  //     +----+     +----+     +----+     +----+
  //     | T4 | --> | Q2 | --> | T5 | --> | Q3 |
  //     +----+     +----+     +----+     +----+
  //
  ktl::array<TestQueue, 4> queues;
  ktl::array<TestThread, 6> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 4, .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 5, .owner_expected = true},
      Action{.thread_index = 4, .queue_index = 2, .owning_thread_index = 5, .owner_expected = true},
      Action{.thread_index = 5,
             .queue_index = 3,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 2u, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u, 2u, 3u};
  constexpr ktl::array expected_owners{0u, 2u, 5u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_target_upstream_from_woken_thread() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //    +----+     +----+     +----+
  //    | T3 | --> | Q1 | --> | T0 | -+
  //    +----+     +----+     +----+  |
  //                                  |
  //                          +----+  V  +----+
  //                          | T1 | --> | Q0 |
  //                          +----+  ^  +----+
  //                                  |
  //                          +----+  |
  //                          | T2 | -+
  //                          +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing no owner for Q1.
  //
  //    +----+     +----+     +----+
  //    | T2 | --> | Q0 | --> | T0 |
  //    +----+     +----+     +----+
  //
  //    +----+     +----+
  //    | T1 | --> | Q1 |
  //    +----+  ^  +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 0, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_target_upstream_from_requeue_thread() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //                          +----+
  //                          | T0 | -+
  //                          +----+  |
  //                                  |
  //    +----+     +----+     +----+  V  +----+
  //    | T3 | --> | Q1 | --> | T1 | --> | Q0 |
  //    +----+     +----+     +----+  ^  +----+
  //                                  |
  //                          +----+  |
  //                          | T2 | -+
  //                          +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing no owner for Q1.
  //
  //    +----+     +----+     +----+
  //    | T2 | --> | Q0 | --> | T0 |
  //    +----+     +----+     +----+
  //
  //    +----+     +----+
  //    | T1 | --> | Q1 |
  //    +----+  ^  +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 1, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_target_upstream_from_still_blocked_thread() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //                          +----+
  //                          | T0 | -+
  //                          +----+  |
  //                                  |
  //                          +----+  V  +----+
  //                          | T1 | --> | Q0 |
  //                          +----+  ^  +----+
  //                                  |
  //    +----+     +----+     +----+  |
  //    | T3 | --> | Q1 | --> | T2 | -+
  //    +----+     +----+     +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing no owner for Q1.
  //
  //    +----+     +----+     +----+
  //    | T2 | --> | Q0 | --> | T0 |
  //    +----+     +----+     +----+
  //
  //    +----+     +----+
  //    | T1 | --> | Q1 |
  //    +----+  ^  +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 2, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_requeue_target_upstream_from_requeue_thread_stays_upstream() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //                          +----+
  //                          | T0 | -+
  //                          +----+  |
  //                                  |
  //    +----+     +----+     +----+  V  +----+
  //    | T3 | --> | Q1 | --> | T1 | --> | Q0 |
  //    +----+     +----+     +----+  ^  +----+
  //                                  |
  //                          +----+  |
  //                          | T2 | -+
  //                          +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing T2 as the new owner for Q1.
  //
  //    +----+     +----+     +----+     +----+     +----+
  //    | T1 | --> | Q1 | --> | T2 | --> | Q0 | --> | T0 |
  //    +----+  ^  +----+     +----+     +----+     +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 1,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 2,
             .queue_index = 0,
             .owning_thread_index = Invalid,
             .owner_expected = true},
      Action{.thread_index = 3, .queue_index = 1, .owning_thread_index = 1, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 2, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, 2u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_wake_queue_upstream_from_requeue_target() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //    +----+
  //    | T0 | -+
  //    +----+  |
  //            |
  //    +----+  V  +----+     +----+     +----+
  //    | T1 | --> | Q0 | --> | T3 | --> | Q1 |
  //    +----+  ^  +----+     +----+     +----+
  //            |
  //    +----+  |
  //    | T2 | -+
  //    +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing no owner for Q1.
  //
  //    +----+     +----+     +----+
  //    | T2 | --> | Q0 | --> | T0 |
  //    +----+     +----+     +----+
  //
  //    +----+     +----+
  //    | T1 | --> | Q1 |
  //    +----+  ^  +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 3,
             .queue_index = 1,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = Invalid, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_wake_queue_upstream_from_requeue_target_swaps_position() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //    +----+
  //    | T0 | -+
  //    +----+  |
  //            |
  //    +----+  V  +----+     +----+     +----+
  //    | T1 | --> | Q0 | --> | T3 | --> | Q1 |
  //    +----+  ^  +----+     +----+     +----+
  //            |
  //    +----+  |
  //    | T2 | -+
  //    +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing T2 for the new owner of Q1.
  //
  //    +----+     +----+     +----+     +----+     +----+
  //    | T1 | --> | Q1 | --> | T2 | --> | Q0 | --> | T0 |
  //    +----+  ^  +----+     +----+     +----+     +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 3,
             .queue_index = 1,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 2, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, 2u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_wake_queue_upstream_from_requeue_target_shares_new_wq_owner() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //    +----+
  //    | T0 | -+
  //    +----+  |
  //            |
  //    +----+  V  +----+     +----+     +----+
  //    | T1 | --> | Q0 | --> | T3 | --> | Q1 |
  //    +----+  ^  +----+     +----+     +----+
  //            |
  //    +----+  |
  //    | T2 | -+
  //    +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing T0 for the new owner of Q1.
  //
  //    +----+     +----+     +----+
  //    | T2 | --> | Q0 | --> | T0 |
  //    +----+     +----+  ^  +----+
  //                       |
  //    +----+     +----+  |
  //    | T1 | --> | Q1 | -+
  //    +----+  ^  +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 3,
             .queue_index = 1,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 0, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, 0u};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

bool owq_topology_test_wake_queue_upstream_from_requeue_target_rejects_new_rq_thread() {
  BEGIN_TEST;

  // TODO(https://fxbug.dev/378976169): Investigate hang on single-cpu builder.
  if (arch_max_num_cpus() == 1) {
    printf("Skipping test that requires more than one cpu.\n");
    END_TEST;
  }

  // Try to go from this:
  //
  //    +----+
  //    | T0 | -+
  //    +----+  |
  //            |
  //    +----+  V  +----+     +----+     +----+
  //    | T1 | --> | Q0 | --> | T3 | --> | Q1 |
  //    +----+  ^  +----+     +----+     +----+
  //            |
  //    +----+  |
  //    | T2 | -+
  //    +----+
  //
  // To this, by doing a wake-1, requeue-1 from Q0 to Q1, assigning the woken
  // thread as the Q0 owner, and choosing T1 for the new owner of Q1.
  //
  //    +----+     +----+     +----+
  //    | T2 | --> | Q0 | --> | T0 |
  //    +----+     +----+     +----+
  //
  //     +----------------------+
  //     |                      |
  //     V   +----+     +----+  |
  //     +-> | T1 | --> | Q1 |--+
  //         +----+  ^  +----+
  //                 |
  //         +----+  |
  //         | T3 | -+
  //         +----+
  //
  // This cycle should be rejected, resulting in the following:
  //
  //    +----+     +----+     +----+
  //    | T2 | --> | Q0 | --> | T0 |
  //    +----+     +----+     +----+
  //
  //    +----+     +----+
  //    | T1 | --> | Q1 |
  //    +----+  ^  +----+
  //            |
  //    +----+  |
  //    | T3 | -+
  //    +----+
  //
  //
  ktl::array<TestQueue, 2> queues;
  ktl::array<TestThread, 4> threads;
  constexpr ktl::array setup_actions{
      Action{.thread_index = 0, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 1, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 2, .queue_index = 0, .owning_thread_index = 3, .owner_expected = true},
      Action{.thread_index = 3,
             .queue_index = 1,
             .owning_thread_index = Invalid,
             .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_index = 1, .assign_woken = true};
  constexpr ktl::array expected_blocking_queues{Invalid, 1u, 0u, 1u};
  constexpr ktl::array expected_owners{0u, Invalid};

  ASSERT_TRUE(OwnedWaitQueueTopologyTests::RunRequeueTest(
      threads, queues, setup_actions, requeue_action, expected_blocking_queues, expected_owners));

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(owq_topology_tests)
UNITTEST("simple", owq_topology_test_simple)
UNITTEST("simple owner change", owq_topology_test_simple_owner_change)
UNITTEST("deny self own", owq_topology_test_deny_self_own)
UNITTEST("owner becomes blocked; no new owner",
         owq_topology_test_owner_becomes_blocked_no_new_owner)
UNITTEST("owner becomes blocked; yes new owner",
         owq_topology_test_owner_becomes_blocked_yes_new_owner)
UNITTEST("owner becomes blocked deny blocked new owner",
         owq_topology_test_owner_becomes_blocked_deny_blocked_new_owner)
UNITTEST("blocked owners", owq_topology_test_blocked_owners)
UNITTEST("blocking thread downstream of owner; no owner change",
         owq_topology_test_blocking_thread_downstream_of_owner_no_owner_change)
UNITTEST("blocking thread downstream of owner; yes owner change",
         owq_topology_test_blocking_thread_downstream_of_owner_yes_owner_change)
UNITTEST("intersecting owner chains", owq_topology_test_intersecting_owner_chains)
UNITTEST("intersecting owner chains; old owner is target of new owner chain",
         owq_topology_test_intersecting_owner_chains_old_owner_target_of_new_owner_chain)
UNITTEST("basic requeue", owq_topology_test_basic_requeue)
UNITTEST("requeue assign woken owner", owq_topology_test_requeue_assign_woken_owner)
UNITTEST("requeue assign both owners", owq_topology_test_requeue_assign_both_owners)
UNITTEST("requeue owner in wake queue", owq_topology_test_requeue_owner_in_wake_queue)
UNITTEST("requeue owner is new wake owner", owq_topology_test_requeue_owner_is_new_wake_owner)
UNITTEST("requeue owner is in requeue target", owq_topology_test_requeue_owner_is_in_requeue_target)
UNITTEST("wake owner becomes requeue owner", owq_topology_test_wake_owner_becomes_requeue_owner)
UNITTEST("wake and requeue have same owner keep rq owner",
         owq_topology_test_wake_and_requeue_have_same_owner_keep_rq_owner)
UNITTEST("wake and requeue have same owner lose rq owner",
         owq_topology_test_wake_and_requeue_have_same_owner_lose_rq_owner)
UNITTEST("requeue op intersecting owner chains no rq owner change",
         owq_topology_test_requeue_op_intersecting_owner_chains_no_rq_owner_change)
UNITTEST("requeue op intersecting owner chains change rqo to none",
         owq_topology_test_requeue_op_intersecting_owner_chains_change_rqo_to_none)
UNITTEST("requeue op intersecting owner chains change rqo to wq thread",
         owq_topology_test_requeue_op_intersecting_owner_chains_change_rqo_to_wq_thread)
UNITTEST("requeue target upstream from woken thread",
         owq_topology_test_requeue_target_upstream_from_woken_thread)
UNITTEST("requeue target upstream from requeue thread",
         owq_topology_test_requeue_target_upstream_from_requeue_thread)
UNITTEST("requeue target upstream from still blocked thread",
         owq_topology_test_requeue_target_upstream_from_still_blocked_thread)
UNITTEST("requeue target upstream from requeue thread stays upstream",
         owq_topology_test_requeue_target_upstream_from_requeue_thread_stays_upstream)
UNITTEST("wake queue upstream from requeue target",
         owq_topology_test_wake_queue_upstream_from_requeue_target)
UNITTEST("wake queue upstream from requeue target swaps position",
         owq_topology_test_wake_queue_upstream_from_requeue_target_swaps_position)
UNITTEST("wake queue upstream from requeue target shares new wq owner",
         owq_topology_test_wake_queue_upstream_from_requeue_target_shares_new_wq_owner)
UNITTEST("wake queue upstream from requeue target rejects new rq thread",
         owq_topology_test_wake_queue_upstream_from_requeue_target_rejects_new_rq_thread)
UNITTEST_END_TESTCASE(owq_topology_tests, "owq", "OwnedWaitQueue topology tests")
