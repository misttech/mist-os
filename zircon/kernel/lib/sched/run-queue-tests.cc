// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/sched/run-queue.h>

#include <array>

#include <gtest/gtest.h>

#include "test-thread.h"

namespace {

using Duration = sched::Duration;
using Time = sched::Time;

TEST(RunQueueTests, Empty) {
  sched::RunQueue<TestThread> queue;
  EXPECT_TRUE(queue.empty());
  EXPECT_EQ(queue.begin(), queue.end());
}

TEST(RunQueueTests, QueueNonExpired) {
  TestThread thread1{{Capacity(5), Period(10)}, Start(0)};
  TestThread thread2{{Capacity(2), Period(10)}, Start(1)};
  TestThread thread3{{Capacity(3), Period(15)}, Start(2)};

  EXPECT_FALSE(thread1.IsQueued());
  EXPECT_FALSE(thread2.IsQueued());
  EXPECT_FALSE(thread3.IsQueued());

  sched::RunQueue<TestThread> queue;
  EXPECT_EQ(0u, queue.size());
  queue.Queue(thread1, Start(0));
  EXPECT_EQ(1u, queue.size());
  queue.Queue(thread2, Start(0));
  EXPECT_EQ(2u, queue.size());
  queue.Queue(thread3, Start(0));
  EXPECT_EQ(3u, queue.size());

  EXPECT_FALSE(queue.empty());
  EXPECT_TRUE(thread1.IsQueued());
  EXPECT_TRUE(thread2.IsQueued());
  EXPECT_TRUE(thread3.IsQueued());

  bool seen1 = false, seen2 = false, seen3 = false;
  for (const auto& thread : queue) {
    seen1 = seen1 || &thread == &thread1;
    seen2 = seen2 || &thread == &thread2;
    seen3 = seen3 || &thread == &thread3;
  }
  EXPECT_TRUE(seen1);
  EXPECT_TRUE(seen2);
  EXPECT_TRUE(seen3);

  //
  // Check that the threads remain in their original activation period.
  //
  EXPECT_EQ(Start(0), thread1.start());
  EXPECT_EQ(Finish(11), thread2.finish());

  EXPECT_EQ(Start(1), thread2.start());
  EXPECT_EQ(Finish(10), thread1.finish());

  EXPECT_EQ(Start(2), thread3.start());
  EXPECT_EQ(Finish(17), thread3.finish());
}

TEST(RunQueueTests, QueueExpired) {
  TestThread thread1{{Capacity(5), Period(10)}, Start(0)};
  TestThread thread2{{Capacity(2), Period(10)}, Start(1)};
  TestThread thread3{{Capacity(3), Period(15)}, Start(2)};

  thread1.Tick(Capacity(5));
  thread2.Tick(Capacity(2));
  thread3.Tick(Capacity(3));
  EXPECT_TRUE(thread1.IsExpired(Start(5)));
  EXPECT_TRUE(thread2.IsExpired(Start(5)));
  EXPECT_TRUE(thread3.IsExpired(Start(5)));

  sched::RunQueue<TestThread> queue;
  queue.Queue(thread1, Start(5));
  queue.Queue(thread2, Start(5));
  queue.Queue(thread3, Start(5));

  //
  // The threads should have been reactivated.
  //
  EXPECT_FALSE(thread1.IsExpired(Start(5)));
  EXPECT_EQ(Start(5), thread1.start());
  EXPECT_EQ(Finish(15), thread1.finish());

  EXPECT_FALSE(thread2.IsExpired(Start(5)));
  EXPECT_EQ(Start(5), thread2.start());
  EXPECT_EQ(Finish(15), thread2.finish());

  EXPECT_FALSE(thread3.IsExpired(Start(5)));
  EXPECT_EQ(Start(5), thread3.start());
  EXPECT_EQ(Finish(20), thread3.finish());
}

TEST(RunQueueTests, Dequeue) {
  TestThread thread1{{Capacity(5), Period(10)}, Start(0)};
  TestThread thread2{{Capacity(2), Period(10)}, Start(1)};
  TestThread thread3{{Capacity(3), Period(15)}, Start(2)};

  sched::RunQueue<TestThread> queue;
  queue.Queue(thread1, Start(0));
  queue.Queue(thread2, Start(0));
  queue.Queue(thread3, Start(0));

  EXPECT_EQ(3u, queue.size());
  queue.Dequeue(thread1);
  EXPECT_EQ(2u, queue.size());
  queue.Dequeue(thread2);
  EXPECT_EQ(1u, queue.size());
  queue.Dequeue(thread3);
  EXPECT_EQ(0u, queue.size());

  EXPECT_TRUE(queue.empty());
  EXPECT_FALSE(thread1.IsQueued());
  EXPECT_FALSE(thread2.IsQueued());
  EXPECT_FALSE(thread3.IsQueued());
}

TEST(RunQueueTests, OrderedByStartTime) {
  TestThread thread1{{Capacity(5), Period(10)}, Start(0)};
  TestThread thread2{{Capacity(2), Period(10)}, Start(1)};
  TestThread thread3{{Capacity(3), Period(15)}, Start(2)};

  sched::RunQueue<TestThread> queue;
  queue.Queue(thread3, Start(0));  // Queuing order should not matter.
  queue.Queue(thread1, Start(0));
  queue.Queue(thread2, Start(0));

  auto it = queue.begin();
  EXPECT_EQ(it.CopyPointer(), &thread1);
  ++it;
  EXPECT_EQ(it.CopyPointer(), &thread2);
  ++it;
  EXPECT_EQ(it.CopyPointer(), &thread3);
  ++it;
  EXPECT_EQ(queue.end(), it);
}

TEST(RunQueueTests, EvaluateNextThread) {
  // * Next is current, because there are no other threads.
  // * No work done in the current activation cycle, so preemption is once
  //   that's done.
  {
    TestThread current{{Capacity(5), Period(10)}, Start(0)};

    sched::RunQueue<TestThread> queue;
    auto [next, preemption] = queue.EvaluateNextThread(current, Start(5));
    EXPECT_EQ(&current, next);
    EXPECT_EQ(preemption, Time{10});
  }

  // * Next is current, because there are no other threads.
  // * Some work done in the current activation cycle, so preemption should
  //   begin once that's done.
  {
    TestThread current{{Capacity(5), Period(10)}, Start(5)};
    current.Tick(Duration{3});

    sched::RunQueue<TestThread> queue;
    auto [next, preemption] = queue.EvaluateNextThread(current, Start(5));
    EXPECT_EQ(&current, next);
    EXPECT_EQ(preemption, Time{7});
  }

  // * Next is current, because the other threads are not yet eligible.
  {
    TestThread current{{Capacity(5), Period(10)}, Start(0)};
    TestThread threadA{{Capacity(2), Period(10)}, Start(10)};
    TestThread threadB{{Capacity(3), Period(15)}, Start(10)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));
    auto [next, preemption] = queue.EvaluateNextThread(current, Start(0));
    EXPECT_EQ(&current, next);
    EXPECT_EQ(preemption, Time{5});
  }

  // * Another thread has same finish time as current and an earlier start.
  //
  // The other should win.
  {
    TestThread current = TestThread{{Capacity(5), Period(9)}, Start(1)};
    TestThread threadA = TestThread{{Capacity(2), Period(10)}, Start(0)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.EvaluateNextThread(current, Start(1));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(preemption, Time{3});

    EXPECT_FALSE(threadA.IsQueued());
    EXPECT_TRUE(current.IsQueued());
  }

  // * Another thread has same finish time as current and a later start.
  //
  // The current should win.
  {
    TestThread current = TestThread{{Capacity(5), Period(10)}, Start(0)};
    TestThread threadA = TestThread{{Capacity(2), Period(9)}, Start(1)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.EvaluateNextThread(current, Start(1));
    EXPECT_EQ(&current, next);
    EXPECT_EQ(preemption, Time{6});

    EXPECT_FALSE(current.IsQueued());
    EXPECT_TRUE(threadA.IsQueued());
  }

  // * Another thread has same start and finish time as current.
  // * Other thread has a lower address.
  //
  // The other should win.
  {
    // Define in an array together to control for address comparison.
    std::array threads = {
        TestThread{{Capacity(2), Period(10)}, Start(0)},  // threadA
        TestThread{{Capacity(5), Period(10)}, Start(0)},  // current
    };

    TestThread& current = threads[1];
    TestThread& threadA = threads[0];

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.EvaluateNextThread(current, Start(0));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(preemption, Time{2});

    EXPECT_FALSE(threadA.IsQueued());
    EXPECT_TRUE(current.IsQueued());
  }

  // * Another thread has same start and finish time as current.
  // * The current thread has a lower address.
  //
  // Current should win.
  {
    // Define in an array together to control for address comparison.
    std::array threads = {
        TestThread{{Capacity(5), Period(10)}, Start(0)},  // current
        TestThread{{Capacity(2), Period(10)}, Start(0)},  // threadA
    };

    TestThread& current = threads[0];
    TestThread& threadA = threads[1];

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.EvaluateNextThread(current, Start(0));
    EXPECT_EQ(&current, next);
    EXPECT_EQ(preemption, Time{5});

    EXPECT_FALSE(current.IsQueued());
    EXPECT_TRUE(threadA.IsQueued());
  }

  // * Next thread has no work yet done.
  {
    TestThread current{{Capacity(5), Period(10)}, Start(0)};
    TestThread threadA{{Capacity(2), Period(10)}, Start(5)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.EvaluateNextThread(current, Start(10));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(preemption, Time{12});

    EXPECT_FALSE(threadA.IsQueued());
    EXPECT_TRUE(current.IsQueued());
    EXPECT_EQ(Start(10), current.start());  // Reactivated.
  }

  // * Next thread has had some work done so far.
  {
    TestThread current{{Capacity(5), Period(10)}, Start(0)};
    TestThread threadA{{Capacity(2), Period(10)}, Start(5)};
    threadA.Tick(Duration{1});

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));

    auto [next, preemption] = queue.EvaluateNextThread(current, Start(10));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(preemption, Time{11});

    EXPECT_FALSE(threadA.IsQueued());
    EXPECT_TRUE(current.IsQueued());
    EXPECT_EQ(Start(10), current.start());  // Reactivated.
  }

  // * Next-to-next thread will become eligible before next thread finishes.
  {
    TestThread current{{Capacity(5), Period(15)}, Start(0)};
    current.Tick(Duration{5});
    TestThread threadA{{Capacity(7), Period(10)}, Start(5)};
    TestThread threadB{{Capacity(3), Period(5)}, Start(7)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    auto [next, preemption] = queue.EvaluateNextThread(current, Start(5));
    EXPECT_EQ(&threadA, next);
    EXPECT_EQ(preemption, threadB.start());
    EXPECT_EQ(preemption, Time{7});
  }

  // The originally scheduled next two threads are now expired: the third is
  // picked.
  {
    TestThread current{{Capacity(3), Period(4)}, Start(0)};
    current.Tick(Duration{3});
    TestThread threadA{{Capacity(7), Period(10)}, Start(1)};
    TestThread threadB{{Capacity(3), Period(5)}, Start(5)};

    sched::RunQueue<TestThread> queue;
    queue.Queue(threadA, Start(0));
    queue.Queue(threadB, Start(0));

    // t=11 is past the two periods of threads A and B. We should see
    // * current reactivated for [11, 15)
    // * threadA reactivated for [11, 21)
    // * threadB reactivated for [11, 16)
    auto [next, preemption] = queue.EvaluateNextThread(current, Start(11));
    EXPECT_EQ(&current, next);
    EXPECT_EQ(preemption, Time{14});
  }
}

}  // namespace
