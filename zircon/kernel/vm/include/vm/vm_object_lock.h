// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_VM_OBJECT_LOCK_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_VM_OBJECT_LOCK_H_

// This file contains helpers and custom wrapped lock types for the purposes of performing the
// transition from VMOs having a single hierarchy lock to having individual locks, as per
// https://fxbug.dev/338300943
//
// Everything here is temporary and only exists to aid this transition. Once complete this file and
// all its contents can be removed, as they should no longer be used.

#include <kernel/mutex.h>

// During the VMO locking transition all lock acquisitions must state whether the lock being
// acquired is the first time a lock in this VMO hierarchy is being acquired, or if a lock in the
// hierarchy is already held and this is a reentrant acquisition.
// This is implemented using the custom lock policy, resulting in the correct way to acquire a VMO
// lock being:
//   Guard<VmoLockType> guard{lock(), VmLockAcquireMode::First};
//  or
//   Guard<VmoLockType> guard{lock{}, VmLockAcquireMode::Reentrant};
// Providing the incorrect acquisition mode will result in a runtime panic, and is logically the
// same as incorrectly performing an AssertHeld.
enum class VmLockAcquireMode : bool {
  First,
  Reentrant,
};

struct VmReentrantMutexPolicy {
  struct State {
    const VmLockAcquireMode mode = VmLockAcquireMode::First;
  };

  // Protects the thread local lock list and validation.
  using ValidationGuard = LockValidationGuard;

  // No special actions are needed during pre-validation.
  template <typename LockType>
  static void PreValidate(LockType*, State*) {}

  // Basic acquire and release operations.
  template <typename LockType>
  static bool Acquire(LockType* lock, State* state) TA_ACQ(lock) {
    if (state->mode == VmLockAcquireMode::First) {
      lock->AcquireFirst();
    } else {
      lock->AcquireReentrant();
    }
    return true;
  }

  template <typename LockType>
  static void Release(LockType* lock, State* state) TA_REL(lock) {
    if (state->mode == VmLockAcquireMode::First) {
      lock->ReleaseFirst();
    } else {
      lock->ReleaseReentrant();
    }
  }

  // Runtime lock assertions.
  template <typename LockType>
  static void AssertHeld(const LockType& lock) TA_ASSERT(lock) {
    lock.AssertHeld();
  }
};

// Limited reentrant mutex that allows for reentrant acquisitions, with the caveat that any given
// acquisition must be explicitly stated as being either the first or a reentrant acquisition. All
// reentrant acquisitions must be released before the first acquisition can.
class TA_CAP("mutex") VmReentrantMutex {
 public:
  constexpr VmReentrantMutex() = default;
  ~VmReentrantMutex() = default;
  void AcquireFirst() TA_ACQ() {
    mutex_.lock().Acquire();
    ASSERT(reentrant_count_ == 0);
  }
  void AcquireReentrant() TA_ACQ() {
    ASSERT(IsHeld());
    reentrant_count_++;
  }
  void ReleaseFirst() TA_REL() {
    ASSERT(reentrant_count_ == 0);
    mutex_.lock().Release();
  }
  void ReleaseReentrant() TA_REL() {
    ASSERT(IsHeld());
    ASSERT(reentrant_count_ > 0);
    reentrant_count_--;
  }
  void AssertHeld() const TA_ASSERT() { mutex_.lock().AssertHeld(); }
  bool IsContested() const { return mutex_.lock().IsContested(); }
  bool IsHeld() const { return mutex_.lock().IsHeld(); }

 private:
  DECLARE_CRITICAL_MUTEX(VmReentrantMutex) mutex_;
  uint64_t reentrant_count_ TA_GUARDED(mutex_) = 0;
};
LOCK_DEP_POLICY(VmReentrantMutex, VmReentrantMutexPolicy);

// An indirection around a VmReentrantMutex that provides an object to wrap a Lock<> around to give
// something for LockDep to track.
class TA_CAP("mutex") VmReentrantMutexRef {
 public:
  // It is the responsibility of the caller to guarantee that |m| outlives this object.
  explicit VmReentrantMutexRef(VmReentrantMutex& m) : mutex_(m) {}
  ~VmReentrantMutexRef() = default;
  void AcquireFirst() const TA_ACQ() { mutex_.AcquireFirst(); }
  void AcquireReentrant() const TA_ACQ() { mutex_.AcquireReentrant(); }
  void ReleaseFirst() const TA_REL() { mutex_.ReleaseFirst(); }
  void ReleaseReentrant() const TA_REL() { mutex_.ReleaseReentrant(); }
  void AssertHeld() const TA_ASSERT() { mutex_.AssertHeld(); }
  bool IsContested() const { return mutex_.IsContested(); }
  bool IsHeld() const { return mutex_.IsHeld(); }

 private:
  VmReentrantMutex& mutex_;
};
LOCK_DEP_POLICY(VmReentrantMutexRef, VmReentrantMutexPolicy);

// A wrapped mutex is just a CriticalMutex that supports the VmReentrantMutexPolicy. In this case it
// does not support reentrancy and treats a reentrant acquisition / release identically to an
// initial one.
class TA_CAP("mutex") VmWrappedMutex {
 public:
  constexpr VmWrappedMutex() = default;
  ~VmWrappedMutex() = default;
  void AcquireFirst() TA_ACQ() { mutex_.Acquire(); }
  void AcquireReentrant() TA_ACQ() { mutex_.Acquire(); }
  void ReleaseFirst() TA_REL() { mutex_.Release(); }
  void ReleaseReentrant() TA_REL() { mutex_.Release(); }
  void AssertHeld() const TA_ASSERT() { mutex_.AssertHeld(); }
  bool IsContested() const { return mutex_.IsContested(); }
  bool IsHeld() const { return mutex_.IsHeld(); }

 private:
  CriticalMutex mutex_;
};
LOCK_DEP_POLICY(VmWrappedMutex, VmReentrantMutexPolicy);

// To allow the VMO code to be as generic as possible across different lock types define traits for
// each of these locks that can be selected between.
namespace vmo_lock_traits {
struct Global {
  using LockType = CriticalMutex;
  static constexpr bool HasLocalLock = false;
  static constexpr bool HasSharedLock = true;
  using SharedLockType = CriticalMutex;
  static constexpr lockdep::LockFlags SharedLockFlags = lockdep::LockFlagsNone;
};
struct Hierarchy {
  using LockType = VmReentrantMutex;
  static constexpr bool HasLocalLock = false;
  static constexpr bool HasSharedLock = true;
  using SharedLockType = VmReentrantMutex;
  static constexpr lockdep::LockFlags SharedLockFlags =
      lockdep::LockFlagsTrackingDisabled | lockdep::LockFlagsNestable;
};
struct TrackedHierarchy {
  using LockType = VmReentrantMutexRef;
  static constexpr bool HasLocalLock = true;
  using LocalLockType = VmReentrantMutexRef;
  static constexpr lockdep::LockFlags LocalLockFlags = lockdep::LockFlagsNestable;
  static constexpr bool HasSharedLock = true;
  using SharedLockType = VmReentrantMutex;
  static constexpr lockdep::LockFlags SharedLockFlags = lockdep::LockFlagsNone;
};
struct Fine {
  using LockType = VmWrappedMutex;
  static constexpr bool HasLocalLock = true;
  using LocalLockType = VmWrappedMutex;
  static constexpr lockdep::LockFlags LocalLockFlags = lockdep::LockFlagsNestable;
  static constexpr bool HasSharedLock = false;
};
}  // namespace vmo_lock_traits

// For simplicity of implementation we preference using the preprocessor for switching the actual
// declarations in the data structures and so some of the trait information gets replicated into
// these #define.
// To minimize errors the lock type is indirectly selected by manipulating these defines, and we
// static assert that they match the traits of the chosen lock.
#define VMO_USE_LOCAL_LOCK false
#define VMO_USE_SHARED_LOCK true

#if VMO_USE_LOCAL_LOCK && VMO_USE_SHARED_LOCK
using VmoLockTraits = vmo_lock_traits::TrackedHierarchy;
#elif VMO_USE_LOCAL_LOCK && !VMO_USE_SHARED_LOCK
using VmoLockTraits = vmo_lock_traits::Fine;
#elif !VMO_USE_LOCAL_LOCK && VMO_USE_SHARED_LOCK
// TODO(https://fxbug.dev/338300943) Switch from Global to Hierarchy to begin the lock transition
// process.
using VmoLockTraits = vmo_lock_traits::Global;
#else
#error "Invalid locking discipline"
#endif

static_assert(VMO_USE_LOCAL_LOCK == VmoLockTraits::HasLocalLock);
static_assert(VMO_USE_SHARED_LOCK == VmoLockTraits::HasSharedLock);

// As it is very commonly used, extract the LockType from the traits into an using to use
// definition.
using VmoLockType = VmoLockTraits::LockType;

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_VM_OBJECT_LOCK_H_
