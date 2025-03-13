// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <vm/vm_object_lock.h>

#include "test_helper.h"

#define VMO_TEST_WILL_DEBUG_ASSERT false
#define VMO_TEST_WILL_NOT_COMPILE false
#define VMO_TEST_WILL_FAIL_VALIDATION false

namespace {

// For testing we want a templated object hierarchy that can be instantiated with all three locking
// disciplines. To achieve this we introduce a few extra template helpers around the
// vmo_lock_traits.
struct Empty {};
using HierarchyLock = vmo_lock_traits::Hierarchy;
using TrackedHierarchyLock = vmo_lock_traits::TrackedHierarchy;
using FineLock = vmo_lock_traits::Fine;

template <typename Traits>
struct LocalLockType {
  using type = Traits::LocalLockType;
  static constexpr lockdep::LockFlags flags = Traits::LocalLockFlags;
};

template <>
struct LocalLockType<vmo_lock_traits::Hierarchy> {
  using type = Empty;
  static constexpr lockdep::LockFlags flags = lockdep::LockFlagsNone;
};

template <typename Traits>
struct SharedLockType {
  using type = Traits::SharedLockType;
  static constexpr lockdep::LockFlags flags = Traits::SharedLockFlags;
};

template <>
struct SharedLockType<vmo_lock_traits::Fine> {
  using type = Empty;
  static constexpr lockdep::LockFlags flags = lockdep::LockFlagsNone;
};

template <typename Traits>
struct State : public fbl::RefCounted<State<Traits>> {
  LOCK_DEP_INSTRUMENT(State, typename SharedLockType<Traits>::type, SharedLockType<Traits>::flags)
  lock;
};

template <typename Traits>
class Object : public fbl::RefCounted<Object<Traits>> {
 public:
  template <typename T = Traits, ktl::enable_if_t<T::HasLocalLock && T::HasSharedLock, bool> = true>
  explicit Object(fbl::RefPtr<State<Traits>> state)
      : shared_state_(ktl::move(state)), lock_(shared_state_->lock.lock()) {}

  template <typename T = Traits,
            ktl::enable_if_t<!T::HasLocalLock || !T::HasSharedLock, bool> = true>
  explicit Object(fbl::RefPtr<State<Traits>> state) : shared_state_(ktl::move(state)) {}

  template <typename T = Traits, ktl::enable_if_t<T::HasLocalLock, bool> = true>
  Lock<typename T::LockType>* lock() const TA_RET_CAP(lock_) {
    return &lock_;
  }

  template <typename T = Traits, ktl::enable_if_t<!T::HasLocalLock, bool> = true>
  Lock<typename T::LockType>* lock() const TA_RET_CAP(&shared_state_->lock) {
    return &shared_state_->lock;
  }

  int data TA_GUARDED(lock()) = 0;

 private:
  const fbl::RefPtr<State<Traits>> shared_state_;
  mutable LOCK_DEP_INSTRUMENT(Object, typename LocalLockType<Traits>::type,
                              LocalLockType<Traits>::flags) lock_;
};

struct LockObject {
  mutable DECLARE_MUTEX(LockObject) lock;
};

// Helper to make a set of objects that reference the same shared state.
template <typename T>
ktl::tuple<fbl::RefPtr<Object<T>>, fbl::RefPtr<Object<T>>, fbl::RefPtr<Object<T>>> make_objects() {
  fbl::AllocChecker ac;
  fbl::RefPtr<State<T>> state = fbl::AdoptRef<State<T>>(new (&ac) State<T>);
  ASSERT(ac.check());
  fbl::RefPtr<Object<T>> o1 = fbl::AdoptRef<Object<T>>(new (&ac) Object<T>(state));
  ASSERT(ac.check());
  fbl::RefPtr<Object<T>> o2 = fbl::AdoptRef<Object<T>>(new (&ac) Object<T>(state));
  ASSERT(ac.check());
  fbl::RefPtr<Object<T>> o3 = fbl::AdoptRef<Object<T>>(new (&ac) Object<T>(state));
  ASSERT(ac.check());

  return {o1, o2, o3};
}

// Common tests that apply to all locking disciplines.
template <typename T>
bool vmo_lock_common() {
  BEGIN_TEST;

  auto [o1, o2, o3] = make_objects<T>();

  // Test that objects can be locked independently, with the default mode if none given being First.
  {
    Guard<typename T::LockType> g1{o1->lock()};
    o1->data = 42;
  }
  {
    Guard<typename T::LockType> g2{o2->lock()};
    o2->data = 42;
  }

  // Clang static analysis will not let allow locking one object and accessing a different one, even
  // if the lock is shared.
  if constexpr (false || VMO_TEST_WILL_NOT_COMPILE) {
    Guard<typename T::LockType> g1{o1->lock()};
    o2->data = 42;
  }

  // Should be able to acquire locks in order, and perform a CallUnlocked on the leaf lock in the
  // sequence.
  {
    Guard<typename T::LockType> g1{AssertOrderedLock, o1->lock(), 1, VmLockAcquireMode::First};
    Guard<typename T::LockType> g2{AssertOrderedLock, o2->lock(), 2, VmLockAcquireMode::Reentrant};
    g2.CallUnlocked([]() {});
  }

  // The root lock may not be dropped (even for a CallUnlocked) while any reentrant locks are still
  // held. This will generate both a runtime assertion, and lock dep lock ordering violation, and is
  // incorrect for all locking types.
  if constexpr (false || VMO_TEST_WILL_DEBUG_ASSERT) {
    Guard<typename T::LockType> g1{AssertOrderedLock, o1->lock(), 1, VmLockAcquireMode::First};
    Guard<typename T::LockType> g2{AssertOrderedLock, o2->lock(), 2, VmLockAcquireMode::Reentrant};
    g1.CallUnlocked([]() {});
  }

  END_TEST;
}

// Tests specific to shared locking disciplines.
template <typename T>
bool vmo_lock_shared_test() {
  BEGIN_TEST;

  auto [o1, o2, o3] = make_objects<T>();
  LockObject lo;

  // Cannot acquire a shared lock initially as reentrant.
  if constexpr (false || VMO_TEST_WILL_DEBUG_ASSERT) {
    Guard<typename T::LockType> g1{o1->lock(), VmLockAcquireMode::Reentrant};
  }

  // Acquiring the lock through different aliases will still trigger lock order violations with
  // other locks.
  {
    Guard<typename T::LockType> g1{o1->lock(), VmLockAcquireMode::First};
    Guard<Mutex> g2{&lo.lock};
  }
  if constexpr (false || VMO_TEST_WILL_FAIL_VALIDATION) {
    Guard<Mutex> g1{&lo.lock};
    Guard<typename T::LockType> g2{o2->lock(), VmLockAcquireMode::First};
  }

  // Cannot perform a First acquisition after a Reentrant, even via a different object, when using a
  // shared lock.
  if constexpr (false || VMO_TEST_WILL_DEBUG_ASSERT) {
    Guard<typename T::LockType> g1{AssertOrderedLock, o1->lock(), 1, VmLockAcquireMode::First};
    Guard<typename T::LockType> g2{AssertOrderedLock, o2->lock(), 2, VmLockAcquireMode::Reentrant};
    Guard<typename T::LockType> g3{AssertOrderedLock, o3->lock(), 3, VmLockAcquireMode::First};
  }

  END_TEST;
}

// Tests for the fine locking discipline.
bool vmo_lock_fine_test() {
  BEGIN_TEST;

  auto [o1, o2, o3] = make_objects<FineLock>();

  // With fine locking it is legal to acquire a lock for the first time in reentrant, since the
  // locks are not actually reentrant and only a single acquisition is allowed anyway.
  {
    Guard<VmWrappedMutex> g1{o1->lock(), VmLockAcquireMode::Reentrant};
  }

  // As per the above comment, the lock is not actually reentrant and so taking the same lock twice
  // should fail both runtime assertions and lockdep validation.
  if constexpr (false || VMO_TEST_WILL_DEBUG_ASSERT) {
    // Use an alias to trick the static analysis so that this compiles.
    fbl::RefPtr<Object<FineLock>> o1_alias = o1;
    Guard<VmWrappedMutex> g1{o1->lock(), VmLockAcquireMode::Reentrant};
    Guard<VmWrappedMutex> g2{o1_alias->lock(), VmLockAcquireMode::Reentrant};
  }

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(vmo_lock_tests)
VM_UNITTEST(vmo_lock_common<HierarchyLock>)
VM_UNITTEST(vmo_lock_common<TrackedHierarchyLock>)
VM_UNITTEST(vmo_lock_common<FineLock>)
VM_UNITTEST(vmo_lock_shared_test<HierarchyLock>)
VM_UNITTEST(vmo_lock_shared_test<TrackedHierarchyLock>)
VM_UNITTEST(vmo_lock_fine_test)
UNITTEST_END_TESTCASE(vmo_lock_tests, "vmo_lock", "VmObject lock tests")
