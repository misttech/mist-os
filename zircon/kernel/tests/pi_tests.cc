// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/unittest/unittest.h>
#include <lib/zircon-internal/macros.h>
#include <lib/zx/time.h>
#include <platform.h>
#include <zircon/types.h>

#include <new>

#include <fbl/alloc_checker.h>
#include <fbl/macros.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/auto_preempt_disabler.h>
#include <kernel/owned_wait_queue.h>
#include <kernel/scheduler.h>
#include <kernel/thread.h>
#include <kernel/wait.h>
#include <ktl/algorithm.h>
#include <ktl/array.h>
#include <ktl/atomic.h>
#include <ktl/iterator.h>
#include <ktl/limits.h>
#include <ktl/type_traits.h>
#include <ktl/unique_ptr.h>

#include "tests.h"

#include <ktl/enforce.h>

namespace {

constexpr SchedWeight TEST_LOWEST_WEIGHT =
    SchedulerState::ConvertPriorityToWeight(LOWEST_PRIORITY + 1);
constexpr SchedWeight TEST_HIGHEST_WEIGHT =
    SchedulerState::ConvertPriorityToWeight(HIGHEST_PRIORITY);
constexpr SchedWeight TEST_DEFAULT_WEIGHT =
    SchedulerState::ConvertPriorityToWeight(DEFAULT_PRIORITY);
constexpr SchedWeight TEST_EPSILON_WEIGHT{ffl::FromRatio<int64_t>(1, SchedWeight::Format::Power)};

class TestThread;  // fwd decl

enum class InheritableProfile { No, Yes };

// An RAII style helper which automatically assigns a deadline profile (with a
// short deadline and high utilization)to a thread, restoring the base profile
// automatically when the test ends.  Many of these tests need to rely on timing
// in order to control the order with which threads time out of various wait
// queues.  Since we don't have deterministic control over timing in our tests,
// we rely on our high priority test thread being scheduled and pre-empting all
// other threads when it's timer goes off in order to reduce the chances of
// timing related flake in the tests.
class AutoProfileBooster {
 public:
  AutoProfileBooster()
      : initial_base_profile_(Thread::Current::Get()->scheduler_state().SnapshotBaseProfile()) {
    constexpr SchedUtilization utilization = SchedUtilization{90} / SchedUtilization{100};
    constexpr SchedDuration deadline{ZX_USEC(200)};
    const SchedulerState::BaseProfile new_base_profile{SchedDeadlineParams{utilization, deadline}};
    Thread::Current::Get()->SetBaseProfile(new_base_profile);
  }

  ~AutoProfileBooster() { Thread::Current::Get()->SetBaseProfile(initial_base_profile_); }

  DISALLOW_COPY_ASSIGN_AND_MOVE(AutoProfileBooster);

 private:
  const SchedulerState::BaseProfile initial_base_profile_;
};

// A small helper which creates different permutations of an input array based
// on a distribution method, and optional random seed. Used for things like
// determining which profiles will be assigned to which test threads, or which
// order threads will be released from the blocked state during  various tests.
class DistroSpec {
 public:
  enum class Type { ASCENDING, DESCENDING, RANDOM, SHUFFLE };
  constexpr DistroSpec(Type t, uint64_t s = 0) : type_(t), seed_(s) {}

  template <typename MemberType, size_t N>
  void Apply(const ktl::array<MemberType, N>& in, ktl::array<MemberType, N>& out) const {
    uint64_t prng = seed_;
    switch (type_) {
      case DistroSpec::Type::ASCENDING:
        for (size_t i = 0; i < N; ++i) {
          out[i] = in[i];
        }
        break;

      case DistroSpec::Type::DESCENDING:
        for (size_t i = 0; i < N; ++i) {
          out[i] = in[in.size() - i - 1];
        }
        break;

      case DistroSpec::Type::RANDOM:
        for (auto& item : out) {
          item = in[rand_r(&prng) % N];
        }
        break;

      // Create a range of values from [0, N) + offset, but shuffle the order of
      // those values in the set.
      case DistroSpec::Type::SHUFFLE:
        // Start by filling our shuffle order array with a illegal sentinel
        // value (N will do the job just fine), then foreach i in the range [0,
        // N) pick a random position in the output to put i, and linearly probe
        // until we find the first unused position in order to shuffle.
        ktl::array<size_t, N> order;
        for (size_t i = 0; i < N; ++i) {
          order[i] = N;
        }

        for (size_t i = 0; i < N; ++i) {
          size_t pos = (rand_r(&prng) % N);
          while (order[pos] != N) {
            pos = (pos + 1) % N;
          }
          order[pos] = i;
        }

        // Finally, produce our output from our input, permuting using the
        // shuffle order.
        for (size_t i = 0; i < N; ++i) {
          out[i] = in[order[i]];
        }
        break;
    }
  }

 private:
  const Type type_;
  const uint64_t seed_;
};

struct ExpectedEffectiveProfile {
  struct {
    SchedDiscipline discipline{SchedDiscipline::Fair};
    SchedWeight fair_weight{0};
    SchedDeadlineParams deadline;
  } base;

  SchedulerState::InheritedProfileValues ipvs;
};
}  // namespace

namespace unittest {
class ThreadEffectiveProfileObserver {
 public:
  void Observe(const Thread& t) {
    Guard<MonitoredSpinLock, IrqSave> thread_lock_guard{ThreadLock::Get(), SOURCE_TAG};
    observed_profile_ = t.scheduler_state().SnapshotEffectiveProfileLocked();
  }

  bool VerifyExpectedEffectiveProfile(const ExpectedEffectiveProfile& eep) {
    BEGIN_TEST;

    bool expected_fair = (eep.base.discipline == SchedDiscipline::Fair) &&
                         (eep.ipvs.uncapped_utilization == SchedUtilization{0});
    ASSERT_EQ(expected_fair, observed_profile_.IsFair());

    if (observed_profile_.IsFair()) {
      SchedWeight expected = eep.base.fair_weight + eep.ipvs.total_weight;
      EXPECT_EQ(expected.raw_value(), observed_profile_.fair.weight.raw_value());
    } else {
      SchedUtilization effective_utilization = eep.ipvs.uncapped_utilization;
      SchedDuration effective_deadline = eep.ipvs.min_deadline;

      if (eep.base.discipline == SchedDiscipline::Deadline) {
        effective_utilization += eep.base.deadline.utilization;
        effective_deadline = ktl::min(effective_deadline, eep.base.deadline.deadline_ns);
      }
      effective_utilization = ktl::min(effective_utilization, SchedUtilization{1});

      SchedDeadlineParams expected{effective_utilization, effective_deadline};
      EXPECT_EQ(expected.capacity_ns.raw_value(),
                observed_profile_.deadline.capacity_ns.raw_value());
      EXPECT_EQ(expected.deadline_ns.raw_value(),
                observed_profile_.deadline.deadline_ns.raw_value());
      EXPECT_EQ(expected.utilization.raw_value(),
                observed_profile_.deadline.utilization.raw_value());
    }

    END_TEST;
  }

 private:
  SchedulerState::EffectiveProfile observed_profile_;
};
}  // namespace unittest

namespace {

class Profile : public fbl::RefCounted<Profile> {
 public:
  virtual ~Profile() = default;
  virtual void Apply(Thread& thread) = 0;
  virtual void SetExpectedBaseProfile(ExpectedEffectiveProfile& eep) = 0;
  virtual void AccumulateExpectedPressure(ExpectedEffectiveProfile& eep) = 0;
  virtual size_t DebugPrint(char* buf, size_t space) = 0;

 protected:
  Profile() = default;
};

class FairProfile : public Profile {
 public:
  static fbl::RefPtr<Profile> Create(SchedWeight weight, InheritableProfile inheritable) {
    fbl::AllocChecker ac;
    FairProfile* profile = new (&ac) FairProfile(weight, inheritable);
    if (ac.check()) {
      return fbl::AdoptRef(profile);
    }
    return nullptr;
  }

  void Apply(Thread& thread) override {
    thread.SetBaseProfile(
        SchedulerState::BaseProfile{weight_, (inheritable_ == InheritableProfile::Yes)});
  }

  void SetExpectedBaseProfile(ExpectedEffectiveProfile& eep) override {
    eep.base.discipline = SchedDiscipline::Fair;
    eep.base.fair_weight = weight_;
    eep.ipvs = SchedulerState::InheritedProfileValues{};
  }

  void AccumulateExpectedPressure(ExpectedEffectiveProfile& eep) override {
    if (inheritable_ == InheritableProfile::Yes) {
      eep.ipvs.total_weight += weight_;
    }
  }

  size_t DebugPrint(char* buf, size_t space) override {
    return snprintf(buf, space, "[weight %ld]", weight_.raw_value());
  }

 private:
  FairProfile(SchedWeight weight, InheritableProfile inheritable)
      : weight_(weight), inheritable_(inheritable) {
    ASSERT(static_cast<uint64_t>(weight_.raw_value()) != 0xFFFFFFFFFFFF0000);
  }

  const SchedWeight weight_;
  const InheritableProfile inheritable_;
};

class DeadlineProfile : public Profile {
 public:
  static fbl::RefPtr<Profile> Create(zx_duration_t capacity, zx_duration_t deadline) {
    fbl::AllocChecker ac;
    DeadlineProfile* profile =
        new (&ac) DeadlineProfile(SchedDuration{capacity}, SchedDuration{deadline});
    if (ac.check()) {
      return fbl::AdoptRef(profile);
    }
    return nullptr;
  }

  void Apply(Thread& thread) override {
    thread.SetBaseProfile(SchedulerState::BaseProfile{sched_params_});
  }

  void SetExpectedBaseProfile(ExpectedEffectiveProfile& eep) override {
    eep.base.discipline = SchedDiscipline::Deadline;
    eep.base.deadline = sched_params_;
    eep.ipvs = SchedulerState::InheritedProfileValues{};
  }

  void AccumulateExpectedPressure(ExpectedEffectiveProfile& eep) override {
    eep.ipvs.uncapped_utilization += sched_params_.utilization;
    eep.ipvs.min_deadline = ktl::min(eep.ipvs.min_deadline, sched_params_.deadline_ns);
  }

  size_t DebugPrint(char* buf, size_t space) override {
    return snprintf(buf, space, "[capacity %ld deadline %ld]",
                    sched_params_.capacity_ns.raw_value(), sched_params_.deadline_ns.raw_value());
  }

 private:
  DeadlineProfile(SchedDuration capacity, SchedDuration deadline)
      : sched_params_(capacity, deadline) {
    DEBUG_ASSERT(capacity <= deadline);
  }

  const SchedDeadlineParams sched_params_;
};

// A simple barrier class which can be waited on by multiple threads.  Used to
// stall test threads at various parts of their execution in order to sequence
// things in a deterministic fashion.
class Barrier {
 public:
  constexpr Barrier(bool signaled = false) : signaled_{signaled} {}
  ~Barrier() {
    Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};
    ASSERT(queue_.IsEmpty());
  }

  void Signal(bool state) {
    bool expected = !state;
    if (signaled_.compare_exchange_strong(expected, state) && state) {
      Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};
      queue_.WakeAll(ZX_OK);
    }
  }

  void Wait(Deadline deadline = Deadline::infinite()) {
    if (state()) {
      return;
    }

    Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};
    if (state()) {
      return;
    }

    queue_.Block(deadline, Interruptible::Yes);
  }

  bool state() const { return signaled_.load(); }

 private:
  ktl::atomic<bool> signaled_;
  WaitQueue queue_;
};

// Helper wrapper for an owned wait queue which manages grabbing and releasing
// the thread lock at appropriate times for us.  Mostly, this is just about
// saving some typing.
class LockedOwnedWaitQueue : public OwnedWaitQueue {
 public:
  constexpr LockedOwnedWaitQueue() = default;
  DISALLOW_COPY_ASSIGN_AND_MOVE(LockedOwnedWaitQueue);

  void ReleaseAllThreads() TA_EXCL(thread_lock) {
    AnnotatedAutoEagerReschedDisabler eager_resched_disabler;
    Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};
    OwnedWaitQueue::WakeThreads(ktl::numeric_limits<uint32_t>::max());
  }

  void ReleaseOneThread() TA_EXCL(thread_lock) {
    AnnotatedAutoEagerReschedDisabler eager_resched_disabler;
    Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};
    auto hook = [](Thread*, void*) { return Hook::Action::SelectAndAssignOwner; };
    OwnedWaitQueue::WakeThreads(1u, {hook, nullptr});
  }
};

// LoopIterPrinter
// A small RAII style class which helps us to print out where a loop iterator
// is when a test fails and bails out.  Note: loop iterator types must be
// convertible to int64_t.
template <typename T>
class LoopIterPrinter {
 public:
  constexpr LoopIterPrinter(const char* field_name, T iter_val)
      : field_name_(field_name), iter_val_(iter_val) {}

  ~LoopIterPrinter() {
    if (field_name_ == nullptr) {
      return;
    }

    char buffer[256];
    size_t offset = 0;

    offset += snprintf(buffer + offset, ktl::size(buffer) - offset,
                       "Test failed with %s == ", field_name_);

    if constexpr (ktl::is_same_v<T, fbl::RefPtr<Profile>>) {
      offset += iter_val_->DebugPrint(buffer + offset, ktl::size(buffer) - offset);
    } else {
      offset += snprintf(buffer + offset, ktl::size(buffer) - offset, "%ld",
                         static_cast<int64_t>(iter_val_));
    }

    printf("%s\n", buffer);
  }

  DISALLOW_COPY_ASSIGN_AND_MOVE(LoopIterPrinter);

  void cancel() { field_name_ = nullptr; }

 private:
  const char* field_name_;
  T iter_val_;
};

#define PRINT_LOOP_ITER(_var_name) LoopIterPrinter print_##_var_name(#_var_name, _var_name)

// The core test thread object.  We use this object to build various graphs of
// priority inheritance chains, and then evaluate that the effective priorities
// of the threads involved in the graph are what we expect them to be after
// various mutations of the graph have taken place.
class TestThread {
 public:
  enum class State : uint32_t {
    INITIAL,
    CREATED,
    WAITING_TO_START,
    STARTED,
    WAITING_FOR_SHUTDOWN,
    SHUTDOWN,
  };

  enum class Condition : uint32_t {
    BLOCKED,
    WAITING_FOR_SHUTDOWN,
  };

  TestThread() = default;
  ~TestThread() { Reset(); }

  DISALLOW_COPY_ASSIGN_AND_MOVE(TestThread);

  // Reset the barrier at the start of a test in order to prevent threads from
  // exiting after they have completed their operation..
  static void ResetShutdownBarrier() { allow_shutdown_.Signal(false); }

  // Clear the barrier and allow shutdown.
  static void ClearShutdownBarrier() { allow_shutdown_.Signal(true); }

  static Barrier& allow_shutdown() { return allow_shutdown_; }

  // Create a thread, settings its entry point and initial profile in
  // the process, but do not start it yet.
  bool Create(fbl::RefPtr<Profile> initial_profile);

  // Start the thread, have it do nothing but wait to be allowed to exit.
  bool DoStall();

  // Start the thread and have it block on an owned wait queue, declaring the
  // specified test thread to be the owner of that queue in the process.
  bool BlockOnOwnedQueue(OwnedWaitQueue* owned_wq, TestThread* owner,
                         zx::duration relative_timeout = zx::duration::infinite());

  // Directly take ownership of the specified wait queue using AssignOwner.
  bool TakeOwnership(OwnedWaitQueue* owned_wq);

  // Reset the thread back to its initial state.  If |explicit_kill| is true,
  // then do not wait for the thread to exit normally if it has been started.
  // Simply send it the kill signal.
  bool Reset(bool explicit_kill = false);

  State state() const { return state_.load(); }
  Profile* initial_profile() const { return initial_profile_.get(); }
  Thread& thread() const {
    DEBUG_ASSERT(thread_ != nullptr);
    return *thread_;
  }

  thread_state tstate() const {
    if (thread_ == nullptr) {
      return thread_state::THREAD_DEATH;
    }

    Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};
    return thread_->state();
  }

  template <Condition condition>
  bool WaitFor();

 private:
  // Test threads in the various tests use lambdas in order to store their
  // customized test operations.  In order to allow these lambda's to capture
  // context from their local scope, but not need to use the heap in order to
  // allocate the storage for the scope, we need to know the worst case
  // capture storage requirements across all of these tests.  Armed with this
  // knowledge, we can use a fit::inline_function to pre-allocate storage in
  // the TestThread object for the worst case lambda we will encounter in the
  // test suite.
  //
  // Currently, this bound is 6 pointer's worth of storage.  If this grows in
  // the future, this constexpr bound should be updated to match the new worst
  // case storage requirement.
  static constexpr size_t kMaxOpLambdaCaptureStorageBytes = sizeof(void*) * 6;

  friend class LockedOwnedWaitQueue;

  int ThreadEntry();

  static Barrier allow_shutdown_;

  Thread* thread_ = nullptr;
  ktl::atomic<State> state_{State::INITIAL};
  fit::inline_function<void(void), kMaxOpLambdaCaptureStorageBytes> op_;
  fbl::RefPtr<Profile> initial_profile_;
};

Barrier TestThread::allow_shutdown_;

bool TestThread::Create(fbl::RefPtr<Profile> initial_profile) {
  BEGIN_TEST;

  ASSERT_NULL(thread_);
  ASSERT_NULL(initial_profile_);
  ASSERT_EQ(state(), State::INITIAL);

  initial_profile_ = initial_profile;
  thread_ = Thread::Create(
      "pi_test_thread",
      [](void* ctx) -> int { return reinterpret_cast<TestThread*>(ctx)->ThreadEntry(); },
      reinterpret_cast<void*>(this), DEFAULT_PRIORITY);

  ASSERT_NONNULL(thread_);

  state_.store(State::CREATED);

  END_TEST;
}

bool TestThread::DoStall() {
  BEGIN_TEST;
  ASSERT_EQ(state(), State::CREATED);
  ASSERT_FALSE(static_cast<bool>(op_));

  op_ = []() {};

  state_.store(State::WAITING_TO_START);
  thread_->Resume();

  ASSERT_TRUE(WaitFor<Condition::BLOCKED>());

  END_TEST;
}

bool TestThread::BlockOnOwnedQueue(OwnedWaitQueue* owned_wq, TestThread* owner,
                                   zx::duration relative_timeout) {
  BEGIN_TEST;
  ASSERT_EQ(state(), State::CREATED);
  ASSERT_FALSE(static_cast<bool>(op_));

  op_ = [owned_wq, owner_thrd = owner ? owner->thread_ : nullptr, relative_timeout]() {
    AnnotatedAutoEagerReschedDisabler eager_resched_disabler;
    Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};

    Deadline timeout = (relative_timeout == zx::duration::infinite())
                           ? Deadline::infinite()
                           : Deadline::after(relative_timeout.get());

    owned_wq->BlockAndAssignOwner(timeout, owner_thrd, ResourceOwnership::Normal,
                                  Interruptible::Yes);
  };

  state_.store(State::WAITING_TO_START);
  thread_->Resume();

  ASSERT_TRUE(WaitFor<Condition::BLOCKED>());

  END_TEST;
}

bool TestThread::Reset(bool explicit_kill) {
  BEGIN_TEST;

  // If we are explicitly killing the thread as part of the test, then we
  // should not expect the shutdown barrier to be cleared.
  if (!explicit_kill) {
    EXPECT_TRUE(allow_shutdown_.state());
  }

  switch (state()) {
    case State::INITIAL:
      break;
    case State::CREATED:
      // Created but not started?  thread_forget seems to be the proper way to
      // cleanup a thread which was never started.
      ASSERT(thread_ != nullptr);
      thread_->Forget();
      thread_ = nullptr;
      break;

    case State::WAITING_TO_START:
    case State::STARTED:
    case State::WAITING_FOR_SHUTDOWN:
    case State::SHUTDOWN:
      // If we are explicitly killing the thread, send it the kill signal now.
      if (explicit_kill) {
        thread_->Kill();
      }

      // The thread should be on its way to termination as we speak.  Attempt to
      // join it with a relatively short timeout.  If this fails, print a
      // warning and try again with an infinite timeout.  Why try with a short
      // timeout and then an infinite timeout?  We might be running in an
      // emulated or virtualized environment and things may take a lot longer
      // that they otherwise would.  By timing out quickly and printing an
      // warning, we can hopefully make it easier for a developer to figure out
      // what's going on in the case where the second join hangs forever.
      constexpr zx_duration_t timeout = ZX_MSEC(500);
      ASSERT(thread_ != nullptr);
      int ret_code;
      const Deadline join_deadline = Deadline::after(timeout);
      zx_status_t res = thread_->Join(&ret_code, join_deadline.when());
      if (res == ZX_ERR_TIMED_OUT) {
        printf("Timed out while joining thread %p, retrying with infinite timeout\n", thread_);
        res = thread_->Join(&ret_code, ZX_TIME_INFINITE);
      }
      if (res != ZX_OK) {
        panic("join of thread %p failed with %d\n", thread_, res);
      }
      thread_ = nullptr;
  }

  state_.store(State::INITIAL);
  op_ = nullptr;
  initial_profile_ = nullptr;
  ASSERT_NULL(thread_);

  END_TEST;
}

int TestThread::ThreadEntry() {
  if (!static_cast<bool>(op_) || (state() != State::WAITING_TO_START)) {
    return -1;
  }

  initial_profile_->Apply(*thread_);
  state_.store(State::STARTED);
  op_();
  state_.store(State::WAITING_FOR_SHUTDOWN);
  allow_shutdown_.Wait();

  state_.store(State::SHUTDOWN);
  op_ = nullptr;

  return 0;
}

template <TestThread::Condition condition>
bool TestThread::WaitFor() {
  BEGIN_TEST;

  constexpr zx_duration_t timeout = ZX_SEC(10);
  constexpr zx_duration_t poll_interval = ZX_USEC(100);
  zx_time_t deadline = current_time() + timeout;

  while (true) {
    if constexpr (condition == Condition::BLOCKED) {
      thread_state cur_state = tstate();

      if (cur_state == THREAD_BLOCKED) {
        break;
      }

      if (cur_state != THREAD_RUNNING) {
        ASSERT_EQ(THREAD_READY, cur_state);
      }
    } else {
      static_assert(condition == Condition::WAITING_FOR_SHUTDOWN);
      if (state() == State::WAITING_FOR_SHUTDOWN) {
        break;
      }
    }

    zx_time_t now = current_time();
    ASSERT_LT(now, deadline);
    Thread::Current::SleepRelative(poll_interval);
  }

  END_TEST;
}

bool pi_test_basic() {
  BEGIN_TEST;

  AutoProfileBooster pboost;
  enum class ReleaseMethod { WAKE = 0, TIMEOUT, KILL };
  constexpr ReleaseMethod REL_METHODS[] = {ReleaseMethod::WAKE, ReleaseMethod::TIMEOUT,
                                           ReleaseMethod::KILL};
  constexpr zx::duration TIMEOUT_RELEASE_DURATION = zx::msec(10);
  constexpr uint32_t RETRY_LIMIT = 100;

  // create the array of profiles we will use during the test, then verify that
  // all of them were successfully allocated before proceeding.
  const ktl::array profiles = {
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT + TEST_EPSILON_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT - TEST_EPSILON_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_DEFAULT_WEIGHT + TEST_EPSILON_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_DEFAULT_WEIGHT - TEST_EPSILON_WEIGHT, InheritableProfile::No),
      DeadlineProfile::Create(ZX_MSEC(2), ZX_MSEC(5)),
      DeadlineProfile::Create(ZX_USEC(200), ZX_MSEC(1)),
  };

  for (auto& profile : profiles) {
    ASSERT_NONNULL(profile);
  }

  // Test every combination of profiles in a test where one thread waits while
  // another thread blocks behind it, applying profile pressure.  Validate that
  // the receiving thread has the proper effective profile after receiving
  // pressure, and that the effective profile relaxes back to the initial
  // profile after the thread applying pressure ceases to do so for each of the
  // various release methods.
  for (auto& blocking_profile : profiles) {
    for (auto& pressure_profile : profiles) {
      for (auto rel_method : REL_METHODS) {
        PRINT_LOOP_ITER(blocking_profile);
        PRINT_LOOP_ITER(pressure_profile);
        PRINT_LOOP_ITER(rel_method);

        uint32_t retry_count = 0;
        bool retry_test;
        do {
          retry_test = false;

          LockedOwnedWaitQueue owq;
          TestThread pressure_thread;
          TestThread blocking_thread;
          ExpectedEffectiveProfile expected_profile;
          unittest::ThreadEffectiveProfileObserver observer;

          auto cleanup = fit::defer([&]() {
            TestThread::ClearShutdownBarrier();
            owq.ReleaseAllThreads();
            pressure_thread.Reset();
            blocking_thread.Reset();
          });

          // Make sure that our default barriers have been reset to their proper
          // initial states.
          TestThread::ResetShutdownBarrier();

          // Create 2 threads, each with the appropriate profile.
          ASSERT_TRUE(blocking_thread.Create(blocking_profile));
          ASSERT_TRUE(pressure_thread.Create(pressure_profile));

          // Start the first thread, wait for it to block, and verify that it's
          // profile is correct (it should not be changed).
          ASSERT_TRUE(blocking_thread.DoStall());
          blocking_profile->SetExpectedBaseProfile(expected_profile);
          observer.Observe(blocking_thread.thread());
          ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

          // Start the second thread, and have it block on the owned wait queue,
          // and declare the blocking thread to be the owner of the queue at the
          // same time.  Then check to be sure that the effective priority of the
          // blocking thread matches what we expect to see.
          zx::duration relative_timeout = (rel_method == ReleaseMethod::TIMEOUT)
                                              ? TIMEOUT_RELEASE_DURATION
                                              : zx::duration::infinite();
          ASSERT_TRUE(pressure_thread.BlockOnOwnedQueue(&owq, &blocking_thread, relative_timeout));

          // Observe the effective profile of the blocking thread, then observe
          // the state of the thread applying pressure.  If this is the TIMEOUT
          // test, the thread *must* still be blocked on |owq| (not timed out yet)
          // in order for the test to be considered valid.  If the thread managed
          // to unblock before we could observe its effective priority, just try
          // again.
          observer.Observe(blocking_thread.thread());
          if (rel_method == ReleaseMethod::TIMEOUT) {
            retry_test = (pressure_thread.tstate() != thread_state::THREAD_BLOCKED) ||
                         (pressure_thread.state() == TestThread::State::WAITING_FOR_SHUTDOWN);
          }

          // Only verify this if we managed to observe the blocked thread's
          // effective profile while the pressure thread was still applying
          // pressure.
          if (!retry_test) {
            pressure_profile->AccumulateExpectedPressure(expected_profile);
            ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));
          }

          // Finally, release the thread from the owned wait queue based on
          // the release method we are testing.  We will either explicitly
          // wake it up, let it time out, or kill the thread outright.
          //
          // Then, verify that the profile drops back down to what it
          // was initially.
          switch (rel_method) {
            case ReleaseMethod::WAKE:
              owq.ReleaseAllThreads();
              break;

            case ReleaseMethod::TIMEOUT:
              // Wait until the pressure thread times out and has exited.
              ASSERT_TRUE(pressure_thread.WaitFor<TestThread::Condition::WAITING_FOR_SHUTDOWN>());
              break;

            case ReleaseMethod::KILL:
              pressure_thread.Reset(true);
              break;
          }

          blocking_profile->SetExpectedBaseProfile(expected_profile);
          observer.Observe(blocking_thread.thread());
          ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));
        } while (retry_test && (++retry_count < RETRY_LIMIT));

        ASSERT_FALSE(retry_test, "Failed timeout race too many times!");

        print_blocking_profile.cancel();
        print_pressure_profile.cancel();
        print_rel_method.cancel();
      }
    }
  }

  END_TEST;
}

bool pi_test_changing_priority() {
  BEGIN_TEST;

  AutoProfileBooster pboost;
  LockedOwnedWaitQueue owq;
  TestThread pressure_thread;
  TestThread blocking_thread;

  auto cleanup = fit::defer([&]() {
    TestThread::ClearShutdownBarrier();
    owq.ReleaseAllThreads();
    pressure_thread.Reset();
    blocking_thread.Reset();
  });

  const ktl::array profiles = {
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT + TEST_EPSILON_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT - TEST_EPSILON_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_DEFAULT_WEIGHT + TEST_EPSILON_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_DEFAULT_WEIGHT - TEST_EPSILON_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::No),
      DeadlineProfile::Create(ZX_MSEC(2), ZX_MSEC(5)),
      DeadlineProfile::Create(ZX_USEC(200), ZX_MSEC(1)),
  };

  for (auto& profile : profiles) {
    ASSERT_NONNULL(profile);
  }

  ExpectedEffectiveProfile expected_profile;
  unittest::ThreadEffectiveProfileObserver observer;

  // Make sure that our default barriers have been reset to their proper
  // initial states.
  TestThread::ResetShutdownBarrier();

  // Create our threads.
  ASSERT_TRUE(blocking_thread.Create(profiles[0]));
  ASSERT_TRUE(pressure_thread.Create(profiles[0]));

  // Start the first thread, wait for it to block, and verify that it's
  // profile is correct (it should not be changed).
  ASSERT_TRUE(blocking_thread.DoStall());
  blocking_thread.initial_profile()->SetExpectedBaseProfile(expected_profile);
  observer.Observe(blocking_thread.thread());
  ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

  // Block the second thread behind the first.
  ASSERT_TRUE(pressure_thread.BlockOnOwnedQueue(&owq, &blocking_thread));
  pressure_thread.initial_profile()->AccumulateExpectedPressure(expected_profile);
  observer.Observe(blocking_thread.thread());
  ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

  // Changing the pressure thread's profile to a number of different profiles,
  // verifying that the pressure felt by the blocking thread changes
  // appropriately as we do.
  for (auto profile : profiles) {
    PRINT_LOOP_ITER(profile);

    profile->Apply(pressure_thread.thread());
    observer.Observe(blocking_thread.thread());

    blocking_thread.initial_profile()->SetExpectedBaseProfile(expected_profile);
    profile->AccumulateExpectedPressure(expected_profile);
    ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

    print_profile.cancel();
  }

  // Release the pressure thread, validate that the priority is what we
  // started with and we are done.
  owq.ReleaseAllThreads();
  blocking_thread.initial_profile()->SetExpectedBaseProfile(expected_profile);
  observer.Observe(blocking_thread.thread());
  ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

  END_TEST;
}

bool pi_test_chain() {
  BEGIN_TEST;

  enum class ReleaseOrder : uint64_t { ASCENDING = 0, DESCENDING };
  struct Link {
    LockedOwnedWaitQueue queue;
    bool active = false;
  };

  const ktl::array profile_deck{
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::No),
      DeadlineProfile::Create(ZX_MSEC(2), ZX_MSEC(5)),
      DeadlineProfile::Create(ZX_USEC(200), ZX_MSEC(1)),
  };
  constexpr size_t CHAIN_LEN = profile_deck.size();

  const ktl::array PRIORITY_GENERATORS = {
      DistroSpec{DistroSpec::Type::ASCENDING},
      DistroSpec{DistroSpec::Type::DESCENDING},
      DistroSpec{DistroSpec::Type::SHUFFLE, 0xbd6f3bfe33d51c8e},
      DistroSpec{DistroSpec::Type::SHUFFLE, 0x857ce1aa3209ecc7},
  };

  const ktl::array RELEASE_ORDERS = {
      DistroSpec{DistroSpec::Type::ASCENDING},
      DistroSpec{DistroSpec::Type::DESCENDING},
      DistroSpec{DistroSpec::Type::SHUFFLE, 0xac8d4a8ed016caf0},
      DistroSpec{DistroSpec::Type::SHUFFLE, 0xb51e76ca5cf20875},
  };

  ktl::array<TestThread, CHAIN_LEN> threads;
  ktl::array<Link, CHAIN_LEN - 1> links;
  ktl::array<size_t, CHAIN_LEN - 1> release_deck;

  for (auto& profile : profile_deck) {
    ASSERT_NONNULL(profile);
  }

  for (size_t i = 0; i < ktl::size(release_deck); ++i) {
    release_deck[i] = i;
  }

  AutoProfileBooster pboost;
  for (uint32_t pgen_ndx = 0; pgen_ndx < ktl::size(PRIORITY_GENERATORS); ++pgen_ndx) {
    PRINT_LOOP_ITER(pgen_ndx);

    // Generate the profile order to use for this pass.
    ktl::array<fbl::RefPtr<Profile>, CHAIN_LEN> profiles;
    PRIORITY_GENERATORS[pgen_ndx].Apply(profile_deck, profiles);

    for (uint32_t ro_ndx = 0; ro_ndx < ktl::size(RELEASE_ORDERS); ++ro_ndx) {
      PRINT_LOOP_ITER(ro_ndx);

      // Generate the order in which we will release the links for this
      // pass
      decltype(release_deck) release_ordering;
      RELEASE_ORDERS[ro_ndx].Apply(release_deck, release_ordering);

      auto cleanup = fit::defer([&]() {
        TestThread::ClearShutdownBarrier();
        for (auto& l : links) {
          l.queue.ReleaseAllThreads();
        }
        for (auto& t : threads) {
          t.Reset();
        }
      });

      // Lambda used to validate the effective profiles of the threads currently
      // involved in the chain.
      auto ValidatePriorities = [&]() -> bool {
        BEGIN_TEST;

        for (size_t tndx = ktl::size(threads); tndx-- > 0;) {
          PRINT_LOOP_ITER(tndx);

          // All threads should either be created, started or waiting for
          // shutdown.  If they are merely created, they have no effective
          // priority to evaluate at the moment, so just skip them.
          const auto& t = threads[tndx];
          const TestThread::State cur_state = t.state();
          if (cur_state == TestThread::State::CREATED) {
            print_tndx.cancel();
            continue;
          }

          if (cur_state != TestThread::State::WAITING_FOR_SHUTDOWN) {
            ASSERT_EQ(TestThread::State::STARTED, cur_state);
          }

          // The effective profile of this thread should be its base profile,
          // plus all of the profile pressure received from the actively linked
          // thread.
          ExpectedEffectiveProfile expected_profile;
          threads[tndx].initial_profile()->SetExpectedBaseProfile(expected_profile);
          for (size_t i = tndx; (i < ktl::size(links)) && (links[i].active); ++i) {
            ASSERT_LT(i + 1, ktl::size(threads));
            threads[i + 1].initial_profile()->AccumulateExpectedPressure(expected_profile);
          }

          unittest::ThreadEffectiveProfileObserver observer;
          observer.Observe(threads[tndx].thread());
          ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

          print_tndx.cancel();
        }

        END_TEST;
      };

      // Make sure that our default barriers have been reset to their proper
      // initial states.
      TestThread::ResetShutdownBarrier();

      // Create our threads.
      static_assert(ktl::size(threads) == ktl::size(profiles));
      for (uint32_t tndx = 0; tndx < ktl::size(threads); ++tndx) {
        PRINT_LOOP_ITER(tndx);
        ASSERT_TRUE(threads[tndx].Create(profiles[tndx]));
        print_tndx.cancel();
      }

      // Start the head of the chain, wait for it to block, then verify that its
      // profile is correct (it should not be changed).
      auto& chain_head = threads[0];
      ASSERT_TRUE(chain_head.DoStall());
      ASSERT_TRUE(ValidatePriorities());

      // Start each of the threads in the chain one at a time.  Make sure that the
      // pressure of the threads in the chain is properly transmitted each time.
      for (uint32_t tndx = 1; tndx < ktl::size(threads); ++tndx) {
        PRINT_LOOP_ITER(tndx);

        auto& link = links[tndx - 1];
        ASSERT_TRUE(threads[tndx].BlockOnOwnedQueue(&link.queue, &threads[tndx - 1]));
        link.active = true;
        ASSERT_TRUE(ValidatePriorities());

        print_tndx.cancel();
      }

      // Tear down the chain according to the release ordering for this
      // pass.  Make sure that the priority properly relaxes for each of
      // the threads as we do so.
      for (auto link_ndx : release_ordering) {
        PRINT_LOOP_ITER(link_ndx);

        ASSERT_LT(link_ndx, ktl::size(links));
        auto& link = links[link_ndx];
        link.queue.ReleaseAllThreads();
        link.active = false;
        ASSERT_TRUE(ValidatePriorities());

        print_link_ndx.cancel();
      }

      print_ro_ndx.cancel();
    }

    print_pgen_ndx.cancel();
  }

  END_TEST;
}

bool pi_test_multi_waiter() {
  BEGIN_TEST;
  AutoProfileBooster pboost;

  const ktl::array profile_deck{
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::No),
      DeadlineProfile::Create(ZX_MSEC(2), ZX_MSEC(5)),
      DeadlineProfile::Create(ZX_USEC(200), ZX_MSEC(1)),
  };
  constexpr size_t WAITER_CNT = profile_deck.size();

  const ktl::array PRIORITY_GENERATORS = {
      DistroSpec{DistroSpec::Type::ASCENDING},
      DistroSpec{DistroSpec::Type::DESCENDING},
      DistroSpec{DistroSpec::Type::RANDOM, 0x87251211471cb789},
      DistroSpec{DistroSpec::Type::SHUFFLE, 0x857ce1aa3209ecc7},
  };

  LockedOwnedWaitQueue blocking_queue;
  TestThread blocking_thread;
  struct Waiter {
    TestThread thread;
    bool started = false;
    bool is_waiting = false;

    void Reset() {
      thread.Reset();
      started = false;
      is_waiting = false;
    }
  };
  ktl::array<Waiter, WAITER_CNT> waiters;

  for (auto bt_profile : profile_deck) {
    PRINT_LOOP_ITER(bt_profile);

    for (uint32_t pgen_ndx = 0; pgen_ndx < ktl::size(PRIORITY_GENERATORS); ++pgen_ndx) {
      PRINT_LOOP_ITER(pgen_ndx);

      // At the end of the tests, success or failure, be sure to clean up.
      auto cleanup = fit::defer([&]() {
        TestThread::ClearShutdownBarrier();
        blocking_queue.ReleaseAllThreads();
        blocking_thread.Reset();
        for (auto& w : waiters) {
          w.Reset();
        }
      });

      // Make sure that our barriers have been reset.
      TestThread::ResetShutdownBarrier();

      // Select the profiles to apply to the waiter threads during this pass.
      ktl::array<fbl::RefPtr<Profile>, WAITER_CNT> profiles;
      PRIORITY_GENERATORS[pgen_ndx].Apply(profile_deck, profiles);

      // Create all of the threads.
      ASSERT_TRUE(blocking_thread.Create(bt_profile));
      for (uint32_t waiter_ndx = 0; waiter_ndx < ktl::size(waiters); ++waiter_ndx) {
        PRINT_LOOP_ITER(waiter_ndx);

        static_assert(ktl::size(waiters) == ktl::size(profiles));
        ASSERT_TRUE(waiters[waiter_ndx].thread.Create(profiles[waiter_ndx]));

        print_waiter_ndx.cancel();
      }

      // Define a small lambda we will use to validate the expected priorities of
      // each of our threads.
      TestThread* current_owner = &blocking_thread;
      auto ValidatePriorities = [&]() -> bool {
        BEGIN_TEST;

        ExpectedEffectiveProfile expected_profile;
        unittest::ThreadEffectiveProfileObserver observer;

        // The expected profile for the current owner of the OWQ should be its
        // base profile, plus all of the pressure from each of the waiting
        // threads.
        ASSERT_NONNULL(current_owner);
        current_owner->initial_profile()->SetExpectedBaseProfile(expected_profile);
        for (const auto& waiter : waiters) {
          if (waiter.is_waiting) {
            waiter.thread.initial_profile()->AccumulateExpectedPressure(expected_profile);
          }
        }

        observer.Observe(current_owner->thread());
        ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

        // Every waiter thread which has started (waiting or not) should be
        // running with its initial profile, unless it happens to currently be
        // the owner of the OWQ.
        for (size_t waiter_ndx = 0; waiter_ndx < ktl::size(waiters); ++waiter_ndx) {
          PRINT_LOOP_ITER(waiter_ndx);

          const Waiter& waiter = waiters[waiter_ndx];
          if (waiter.started && (&waiter.thread != current_owner)) {
            waiter.thread.initial_profile()->SetExpectedBaseProfile(expected_profile);
            observer.Observe(waiter.thread.thread());
            ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));
          }

          print_waiter_ndx.cancel();
        }

        END_TEST;
      };

      // Start the blocking thread.
      ASSERT_TRUE(blocking_thread.DoStall());
      ASSERT_TRUE(ValidatePriorities());

      // Start each of the threads and have them block on the blocking_queue,
      // declaring blocking_thread to be the owner as they go.  Verify all of
      // the threads' effective profiles as we go.
      for (uint32_t waiter_ndx = 0; waiter_ndx < ktl::size(waiters); ++waiter_ndx) {
        PRINT_LOOP_ITER(waiter_ndx);

        auto& w = waiters[waiter_ndx];
        ASSERT_TRUE(w.thread.BlockOnOwnedQueue(&blocking_queue, current_owner));
        w.started = true;
        w.is_waiting = true;
        ASSERT_TRUE(ValidatePriorities());

        print_waiter_ndx.cancel();
      }

      // Now wake the threads, one at a time, assigning ownership to the thread
      // which was woken each time.  Note that we should not be assuming which
      // thread is going to be woken.  We will need to request that a thread be
      // woken, then figure out after the fact which one was.
      for (uint32_t tndx = 0; tndx < ktl::size(waiters); ++tndx) {
        PRINT_LOOP_ITER(tndx);

        blocking_queue.ReleaseOneThread();

        TestThread* new_owner = nullptr;
        zx_time_t deadline = current_time() + ZX_SEC(10);
        while (current_time() < deadline) {
          for (auto& w : waiters) {
            // If the waiter's is_waiting flag is set, but the thread has
            // reached the WAITING_FOR_SHUTDOWN state, then we know that
            // this was a thread which was just woken.
            if (w.is_waiting && (w.thread.state() == TestThread::State::WAITING_FOR_SHUTDOWN)) {
              new_owner = &w.thread;
              w.is_waiting = false;
              break;
            }
          }

          if (new_owner != nullptr) {
            break;
          }

          Thread::Current::SleepRelative(ZX_USEC(100));
        }

        // Sanity checks.  Make sure that the new owner exists, and is not the
        // same as the old owner.  Also make sure that none of the other threads
        // have been released but have not been recognized yet.
        ASSERT_NONNULL(new_owner);
        ASSERT_NE(new_owner, current_owner);
        for (auto& w : waiters) {
          if (w.is_waiting) {
            ASSERT_EQ(TestThread::State::STARTED, w.thread.state());
          } else {
            ASSERT_EQ(TestThread::State::WAITING_FOR_SHUTDOWN, w.thread.state());
          }
        }
        current_owner = new_owner;

        // Validate our profiles.
        ASSERT_TRUE(ValidatePriorities());

        print_tndx.cancel();
      }

      print_pgen_ndx.cancel();
    }
    print_bt_profile.cancel();
  }

  END_TEST;
}

bool pi_test_multi_owned_queues() {
  BEGIN_TEST;
  AutoProfileBooster pboost;

  const ktl::array profile_deck{
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::Yes),
      FairProfile::Create(TEST_DEFAULT_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_LOWEST_WEIGHT, InheritableProfile::No),
      FairProfile::Create(TEST_HIGHEST_WEIGHT, InheritableProfile::No),
      DeadlineProfile::Create(ZX_MSEC(2), ZX_MSEC(5)),
      DeadlineProfile::Create(ZX_USEC(200), ZX_MSEC(1)),
  };
  constexpr size_t QUEUE_CNT = profile_deck.size();

  struct Waiter {
    TestThread thread;
    LockedOwnedWaitQueue queue;
    bool is_started = false;
    bool is_waiting = false;

    void Reset() {
      queue.ReleaseAllThreads();
      thread.Reset();
      is_started = false;
      is_waiting = false;
    }
  };

  TestThread blocking_thread;
  ktl::array<Waiter, QUEUE_CNT> queues;

  const ktl::array PRIORITY_GENERATORS = {
      DistroSpec{DistroSpec::Type::ASCENDING},
      DistroSpec{DistroSpec::Type::DESCENDING},
      DistroSpec{DistroSpec::Type::RANDOM, 0xb89e3b7442b95a1c},
      DistroSpec{DistroSpec::Type::SHUFFLE, 0x06ec82d4ade8efba},
  };

  for (auto bt_profile : profile_deck) {
    PRINT_LOOP_ITER(bt_profile);

    for (uint32_t pgen_ndx = 0; pgen_ndx < ktl::size(PRIORITY_GENERATORS); ++pgen_ndx) {
      PRINT_LOOP_ITER(pgen_ndx);

      // At the end of the tests, success or failure, be sure to clean up.
      auto cleanup = fit::defer([&]() {
        TestThread::ClearShutdownBarrier();
        blocking_thread.Reset();
        for (auto& q : queues) {
          q.Reset();
        }
      });

      // Make sure that our barriers have been reset.
      TestThread::ResetShutdownBarrier();

      // Select the profiles to apply to the waiter threads during this pass.
      ktl::array<fbl::RefPtr<Profile>, QUEUE_CNT> profiles;
      PRIORITY_GENERATORS[pgen_ndx].Apply(profile_deck, profiles);

      // Create all of the threads.
      ASSERT_TRUE(blocking_thread.Create(bt_profile));
      for (uint32_t queue_ndx = 0; queue_ndx < ktl::size(queues); ++queue_ndx) {
        PRINT_LOOP_ITER(queue_ndx);
        ASSERT_TRUE(queues[queue_ndx].thread.Create(profiles[queue_ndx]));
        print_queue_ndx.cancel();
      }

      // Define a small lambda we will use to validate the expected priorities of
      // each of our threads.
      auto ValidatePriorities = [&]() -> bool {
        BEGIN_TEST;

        ExpectedEffectiveProfile expected_profile;
        unittest::ThreadEffectiveProfileObserver observer;

        // Each of the started queue threads (waiting or not) should simply have their
        // base profile.  Verify this.
        for (uint32_t queue_ndx = 0; queue_ndx < ktl::size(queues); ++queue_ndx) {
          PRINT_LOOP_ITER(queue_ndx);
          const auto& q = queues[queue_ndx];

          if (q.is_started) {
            q.thread.initial_profile()->SetExpectedBaseProfile(expected_profile);
            observer.Observe(q.thread.thread());
            ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));
          }

          print_queue_ndx.cancel();
        }

        // The effective profile of the blocking_thread should be the
        // combination of its base profile, in addition to all of the currently
        // blocked threads whose OWQs are owned by the blocking thread.
        blocking_thread.initial_profile()->SetExpectedBaseProfile(expected_profile);
        for (const auto& q : queues) {
          if (q.is_waiting) {
            q.thread.initial_profile()->AccumulateExpectedPressure(expected_profile);
          }
        }

        observer.Observe(blocking_thread.thread());
        ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

        END_TEST;
      };

      // Start the blocking thread.
      ASSERT_TRUE(blocking_thread.DoStall());
      ASSERT_TRUE(ValidatePriorities());

      // Start each of the threads and have them block on their associated
      // queue, declaring blocking_thread to be the owner of their queue
      // as they go.  Validate priorities at each step.
      for (uint32_t queue_ndx = 0; queue_ndx < ktl::size(queues); ++queue_ndx) {
        PRINT_LOOP_ITER(queue_ndx);

        auto& q = queues[queue_ndx];
        ASSERT_TRUE(q.thread.BlockOnOwnedQueue(&q.queue, &blocking_thread));
        q.is_started = true;
        q.is_waiting = true;
        ASSERT_TRUE(ValidatePriorities());

        print_queue_ndx.cancel();
      }

      // Now wake the threads, one at a time, verifying priorities as we
      // go.
      for (uint32_t queue_ndx = 0; queue_ndx < ktl::size(queues); ++queue_ndx) {
        PRINT_LOOP_ITER(queue_ndx);

        auto& q = queues[queue_ndx];
        q.queue.ReleaseOneThread();
        q.is_waiting = false;
        ASSERT_TRUE(ValidatePriorities());

        print_queue_ndx.cancel();
      }

      print_pgen_ndx.cancel();
    }
    print_bt_profile.cancel();
  }

  END_TEST;
}

template <InheritableProfile kInheritableProfiles>
bool pi_test_cycle() {
  BEGIN_TEST;
  AutoProfileBooster pboost;

  // Deliberately create a cycle and make sure that we don't hang or otherwise
  // exhibit bad behavior.
  struct Link {
    TestThread thread;
    LockedOwnedWaitQueue link;
    fbl::RefPtr<Profile> profile;
  };

  static constexpr size_t CYCLE_LEN = 4;
  ktl::array<Link, CYCLE_LEN> nodes;

  // At the end of the tests, success or failure, be sure to clean up.
  auto cleanup = fit::defer([&]() {
    TestThread::ClearShutdownBarrier();
    for (auto& n : nodes) {
      n.link.ReleaseAllThreads();
    }
    for (auto& n : nodes) {
      n.thread.Reset();
    }
  });

  // Create each of the profiles and assign them to each of the threads.
  for (uint32_t tndx = 0; tndx < ktl::size(nodes); ++tndx) {
    PRINT_LOOP_ITER(tndx);

    SchedWeight tgt_weight = TEST_LOWEST_WEIGHT + (tndx * TEST_EPSILON_WEIGHT);
    ASSERT_LE(tgt_weight.raw_value(), TEST_HIGHEST_WEIGHT.raw_value());
    nodes[tndx].profile = FairProfile::Create(tgt_weight, kInheritableProfiles);
    ASSERT_NONNULL(nodes[tndx].profile);
    ASSERT_TRUE(nodes[tndx].thread.Create(nodes[tndx].profile));

    print_tndx.cancel();
  }

  // Let each thread run, blocking it on its own link and declaring the next
  // thread in the list to be the owner of the link.  When we hit the last
  // thread, we attempt to form a cycle.
  //
  // As of today, the OwnedWaitQueue code will refuse to create the cycle and
  // will "fix" the problem by allowing the thread to block, but declaring the
  // owner of the OWQ the thread is blocking in to be no one.  Our threads are
  // in ascending priority order, so we should not see any changes to the
  // effective priority of any of the threads.
  //
  // Eventually, however, the Block operation will completely fail instead of
  // allowing the cycle to come into existence.  It will not change the owner,
  // nor will it block the thread in question. Instead, it will return an error.
  // This test will need to be updated when that day arrives.
  for (size_t tndx = 0; tndx < ktl::size(nodes); ++tndx) {
    PRINT_LOOP_ITER(tndx);

    TestThread& owner_thread = nodes[(tndx + 1) % ktl::size(nodes)].thread;
    LockedOwnedWaitQueue& link = nodes[tndx].link;
    ASSERT_TRUE(nodes[tndx].thread.BlockOnOwnedQueue(&link, &owner_thread));

    for (size_t validation_ndx = 0; validation_ndx <= tndx; ++validation_ndx) {
      PRINT_LOOP_ITER(validation_ndx);

      // The profile of each link in the chain should be the combination of its
      // base profile and all of the threads in the chain which are blocked
      // behind it.
      ExpectedEffectiveProfile expected_profile;
      nodes[validation_ndx].thread.initial_profile()->SetExpectedBaseProfile(expected_profile);
      for (size_t i = 1; i <= validation_ndx; ++i) {
        TestThread& thread = nodes[validation_ndx - i].thread;
        thread.initial_profile()->AccumulateExpectedPressure(expected_profile);
      }

      unittest::ThreadEffectiveProfileObserver observer;
      observer.Observe(nodes[validation_ndx].thread.thread());
      ASSERT_TRUE(observer.VerifyExpectedEffectiveProfile(expected_profile));

      // Every OWQ in the test vector should be owned by the thread after it in
      // the sequence, except for the last OWQ.  We tried to assign it to be
      // owned by the first thread when we tried to create the cycle, but the
      // implementation should have refused and left the queue with now owner.
      Thread* expected_owner =
          (tndx < (nodes.size() - 1)) ? &(nodes[tndx + 1].thread.thread()) : nullptr;
      Thread* observed_owner;
      {
        Guard<MonitoredSpinLock, IrqSave> guard{ThreadLock::Get(), SOURCE_TAG};
        observed_owner = nodes[tndx].link.owner();
      }
      ASSERT_EQ(expected_owner, observed_owner);

      print_validation_ndx.cancel();
    }

    print_tndx.cancel();
  }

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(pi_tests)
UNITTEST("basic", pi_test_basic)
UNITTEST("changing priority", pi_test_changing_priority)
UNITTEST("chains", pi_test_chain)
UNITTEST("multiple waiters", pi_test_multi_waiter)
UNITTEST("multiple owned queues", pi_test_multi_owned_queues)
UNITTEST("cycles (inheritable)", pi_test_cycle<InheritableProfile::Yes>)
UNITTEST("cycles (non-inheritable)", pi_test_cycle<InheritableProfile::No>)
UNITTEST_END_TESTCASE(pi_tests, "pi", "Priority inheritance tests for OwnedWaitQueues")
