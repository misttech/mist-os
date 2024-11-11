// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// NOTE: Based on ref_counted_upgradeable_tests.cc

#include <lib/mistos/memory/weak_ptr.h>
#include <lib/unittest/unittest.h>

#include <fbl/ref_counted_upgradeable.h>
#include <kernel/event.h>
#include <kernel/thread.h>

namespace unit_testing {
namespace {

using mtl::WeakPtr;
using mtl::WeakPtrFactory;

template <bool EnableAdoptionValidator>
class RawUpgradeTester
    : public fbl::RefCountedUpgradeable<RawUpgradeTester<EnableAdoptionValidator>,
                                        EnableAdoptionValidator> {
 public:
  RawUpgradeTester(fbl::Mutex* mutex, ktl::atomic<bool>* destroying, Event* event)
      : mutex_(mutex), destroying_(destroying), destroying_event_(event), weak_factory_(this) {}

  ~RawUpgradeTester() {
    weak_factory_.InvalidateWeakPtrs();
    atomic_store(destroying_, true);
    if (destroying_event_)
      destroying_event_->Signal(ZX_OK);
    fbl::AutoLock al(mutex_);
  }

 private:
  fbl::Mutex* mutex_;
  ktl::atomic<bool>* destroying_;
  Event* destroying_event_;

 public:
  WeakPtrFactory<RawUpgradeTester> weak_factory_;  // must be last
};

template <bool EnableAdoptionValidator>
int adopt_and_reset(void* arg) {
  fbl::RefPtr<RawUpgradeTester<EnableAdoptionValidator>> rc_client =
      fbl::AdoptRef(reinterpret_cast<RawUpgradeTester<EnableAdoptionValidator>*>(arg));
  // The reset() which will call the dtor, which we expect to
  // block because upgrade_fail_test() is holding the mutex.
  rc_client.reset();
  return 0;
}

template <bool EnableAdoptionValidator>
bool lock_fail_test() {
  BEGIN_TEST;

  fbl::Mutex mutex;
  fbl::AllocChecker ac;
  ktl::atomic<bool> destroying{false};
  Event destroying_event;

  auto raw =
      new (&ac) RawUpgradeTester<EnableAdoptionValidator>(&mutex, &destroying, &destroying_event);
  EXPECT_TRUE(ac.check());

  Thread* thread;
  {
    fbl::AutoLock al(&mutex);
    thread = Thread::Create("", &adopt_and_reset<EnableAdoptionValidator>, raw, DEFAULT_PRIORITY);
    thread->Resume();
    // Wait until the thread is in the destructor.
    ASSERT_OK(destroying_event.Wait());
    EXPECT_TRUE(atomic_load(&destroying));
    // The RawUpgradeTester must be blocked in the destructor, the Lock will fail.
    EXPECT_NULL(raw->weak_factory_.GetWeakPtr().Lock());
    // Verify that the previous Lock attempt did not change the refcount.
    EXPECT_NULL(raw->weak_factory_.GetWeakPtr().Lock());
  }

  int out_code = 0;
  thread->Join(&out_code, ZX_TIME_INFINITE);

  END_TEST;
}

template <bool EnableAdoptionValidator>
bool lock_success_test() {
  BEGIN_TEST;

  fbl::Mutex mutex;
  fbl::AllocChecker ac;
  ktl::atomic<bool> destroying{false};

  auto ref = fbl::AdoptRef(
      new (&ac) RawUpgradeTester<EnableAdoptionValidator>(&mutex, &destroying, nullptr));
  EXPECT_TRUE(ac.check());
  auto raw = ref.get();

  {
    fbl::AutoLock al(&mutex);
    // RawUpgradeTester is not in the destructor so the upgrade should
    // succeed.
    EXPECT_TRUE(raw->weak_factory_.GetWeakPtr().Lock());
  }

  ref.reset();
  EXPECT_TRUE(atomic_load(&destroying));

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_memory_weakptr_threaded)
UNITTEST("lock fail adopt validation on", unit_testing::lock_fail_test<true>)
UNITTEST("lock fail adopt validation off", unit_testing::lock_fail_test<false>)
UNITTEST("lock success adopt validation on", unit_testing::lock_success_test<true>)
UNITTEST("lock success adopt validation off", unit_testing::lock_success_test<false>)
UNITTEST_END_TESTCASE(mistos_memory_weakptr_threaded, "mistos_memory_weakptr_threaded",
                      "Tests MTL WeakPtr (thread)")
