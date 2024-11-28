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

    static SchedDuration DeadlineForNdx(size_t ndx) {
      constexpr SchedDuration kBaseDuration{ZX_USEC(500)};
      constexpr SchedDuration kDurationInc{ZX_USEC(10)};
      return kBaseDuration + (ndx * kDurationInc);
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
    Thread* thread() { return thread_; }
    const OwnedWaitQueue* target_queue() const { return target_queue_; }
    const OwnedWaitQueue* blocking_queue() const TA_EXCL(chainlock_transaction_token) {
      ASSERT(thread_ != nullptr);
      SingleChainLockGuard guard{
          IrqSaveOption, thread_->get_lock(),
          CLT_TAG("OwnedWaitQueueTopologyTests::TestThread::blocking_queue")};
      return OwnedWaitQueue::DowncastToOwq(thread_->wait_queue_state().blocking_wait_queue());
    }

   private:
    int Main();

    static inline WaitQueue final_exit_queue_;

    ktl::atomic<bool> exit_now_{false};
    ktl::atomic<bool> do_baao_{false};
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

      ASSERT_NULL(owq_);
      fbl::AllocChecker ac;
      owq_.reset(new (&ac) OwnedWaitQueue{});
      ASSERT_TRUE(ac.check());

      END_TEST;
    }

    OwnedWaitQueue* owq() { return owq_.get(); }
    Thread* owner() {
      ASSERT(owq_ != nullptr);
      SingleChainLockGuard guard{IrqSaveOption, owq_->get_lock(),
                                 CLT_TAG("OwnedWaitQueueTopologyTests::TestQueue::owner")};
      return owq_->owner();
    }

   private:
    ktl::unique_ptr<OwnedWaitQueue> owq_;
  };

  struct Action {
    uint32_t t_ndx;       // ndx of the blocking thread.
    uint32_t bq_ndx;      // ndx of the queue which the thread blocks behind
    uint32_t ot_ndx;      // ndx of the thread declared to be the owner.
    bool owner_expected;  // true if we expect the owner to be accepted, false if
                          // we expect the owner to be rejected (and end up with
                          // no owner after the action.
  };

  struct RequeueAction {
    uint32_t wake_count;
    uint32_t requeue_count;
    uint32_t requeue_owner_ndx;
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
      TestThread& t = threads[i];
      if (!setup_for_requeue_test) {
        ASSERT_TRUE(t.Init());
      } else {
        const SchedDuration deadline = TestThread::DeadlineForNdx(i);
        const SchedDuration capacity = deadline / 2;
        const SchedDeadlineParams params{capacity, deadline};
        const SchedulerState::BaseProfile profile{params};
        ASSERT_TRUE(t.Init(&profile));
      }
    }

    for (TestQueue& q : queues) {
      ASSERT_TRUE(q.Init());
    }

    // Perform each of the actions, blocking a given thread behind a given wait
    // queue and optionally declaring an owner as we do.  Afterwards, verify that
    // each of the threads is blocked behind the queue we expect, and that the
    // owner is who we expect.
    for (const Action& a : actions) {
      // Make sue the action indexes are valid.
      ASSERT_LT(a.t_ndx, threads.size());
      ASSERT_LT(a.bq_ndx, queues.size());

      TestThread& blocking_thread = threads[a.t_ndx];
      TestQueue& target_queue = queues[a.bq_ndx];
      TestThread* new_owner{nullptr};
      if (a.ot_ndx != Invalid) {
        ASSERT_LT(a.ot_ndx, threads.size());
        new_owner = &threads[a.ot_ndx];
      }

      // Then perform the block operation.
      ASSERT_TRUE(blocking_thread.DoBlock(target_queue, new_owner));

      // Now validate everything.
      Thread* expected_owner =
          a.owner_expected && (new_owner != nullptr) ? new_owner->thread() : nullptr;
      EXPECT_EQ(expected_owner, target_queue.owner());
      for (const TestThread& t : threads) {
        EXPECT_EQ(t.target_queue(), t.blocking_queue());
      }
    }

    if (setup_for_requeue_test) {
      // Sleep for longer than the longest relative deadline we assigned to our
      // threads.  This will ensure that none of the currently blocked threads
      // have an active deadline (all of their deadlines will have expired).
      // Since the relative deadlines of the threads were assigned in increasing
      // order relative to their index in the test thread array, this means that
      // when it comes time to choose a thread to wake up from the blocked
      // queue, we should always end up choosing the thread in the queue with
      // the lowest index in the test thread array.
      Thread::Current::SleepRelative(TestThread::DeadlineForNdx(threads.size()).raw_value());
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

    auto NdxToThread = [&](uint32_t ndx) -> Thread* {
      if (ndx == Invalid) {
        return nullptr;
      }
      DEBUG_ASSERT(ndx < threads.size());
      return threads[ndx].thread();
    };

    auto NdxToQueue = [&](uint32_t ndx) -> OwnedWaitQueue* {
      if (ndx == Invalid) {
        return nullptr;
      }
      DEBUG_ASSERT(ndx < queues.size());
      return queues[ndx].owq();
    };

    static_assert(QueueCount >= 2, "Requeue tests must always involve at least two queues");

    // Setup the pre-requeue state.
    ASSERT_TRUE(RunBAAOTest(threads, queues, setup_actions, true));

    // Now perform the requeue action
    Thread* const new_requeue_owner = NdxToThread(requeue_action.requeue_owner_ndx);
    OwnedWaitQueue::IWakeRequeueHook& hooks = OwnedWaitQueue::default_wake_hooks();
    OwnedWaitQueue& src_queue = *queues[0].owq();
    OwnedWaitQueue& dst_queue = *queues[1].owq();
    src_queue.WakeAndRequeue(dst_queue, new_requeue_owner, requeue_action.wake_count,
                             requeue_action.requeue_count, hooks, hooks,
                             requeue_action.assign_woken ? OwnedWaitQueue::WakeOption::AssignOwner
                                                         : OwnedWaitQueue::WakeOption::None);

    // Now verify that all of the threads are blocked (or not) in the proper
    // queues, and that all queues are owned by the expected threads (or no
    // thread).
    for (size_t tndx = 0; tndx < expected_blocking_queues.size(); ++tndx) {
      const uint32_t qndx = expected_blocking_queues[tndx];
      OwnedWaitQueue* const expected_queue = NdxToQueue(qndx);
      EXPECT_EQ(expected_queue, threads[tndx].blocking_queue());
    }

    for (size_t qndx = 0; qndx < expected_owners.size(); ++qndx) {
      const uint32_t tndx = expected_owners[qndx];
      Thread* const expected = NdxToThread(tndx);
      EXPECT_EQ(expected, queues[qndx].owner());
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
  ASSERT_NONNULL(queue.owq());
  if (new_owner) {
    ASSERT_NONNULL(new_owner->thread());
    target_owner_ = new_owner->thread();
  }
  target_queue_ = queue.owq();
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 2, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 0, .owner_expected = false},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 2, .owner_expected = true},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 0, .owner_expected = false},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 1, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 2, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 0, .ot_ndx = 2, .owner_expected = true},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 1, .ot_ndx = 2, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = false},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 1, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 0, .ot_ndx = 2, .owner_expected = false},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 1, .ot_ndx = 2, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 2, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 4, .bq_ndx = 2, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 5, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 1, .ot_ndx = 2, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 2, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 4, .bq_ndx = 0, .ot_ndx = 1, .owner_expected = true},
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = false};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 3, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 2, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 0, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 1, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 3, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 4, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 4, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 4, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 5, .owner_expected = true},
      Action{.t_ndx = 4, .bq_ndx = 2, .ot_ndx = 5, .owner_expected = true},
      Action{.t_ndx = 5, .bq_ndx = 3, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 5, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 5, .owner_expected = true},
      Action{.t_ndx = 4, .bq_ndx = 2, .ot_ndx = 5, .owner_expected = true},
      Action{.t_ndx = 5, .bq_ndx = 3, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 4, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 5, .owner_expected = true},
      Action{.t_ndx = 4, .bq_ndx = 2, .ot_ndx = 5, .owner_expected = true},
      Action{.t_ndx = 5, .bq_ndx = 3, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 2u, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 0, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 1, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 2, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = Invalid, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = 1, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 2, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = Invalid, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 2, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 0, .assign_woken = true};
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
      Action{.t_ndx = 0, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 1, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 2, .bq_ndx = 0, .ot_ndx = 3, .owner_expected = true},
      Action{.t_ndx = 3, .bq_ndx = 1, .ot_ndx = Invalid, .owner_expected = true},
  };
  constexpr RequeueAction requeue_action = {
      .wake_count = 1, .requeue_count = 1, .requeue_owner_ndx = 1, .assign_woken = true};
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
