// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_KCONCURRENT_INCLUDE_LIB_KCONCURRENT_CHAINLOCK_H_
#define ZIRCON_KERNEL_LIB_KCONCURRENT_INCLUDE_LIB_KCONCURRENT_CHAINLOCK_H_

#include <lib/concurrent/chainlock.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <zircon/compiler.h>

#include <arch/arch_interrupt.h>
#include <arch/ops.h>
#include <fbl/null_lock.h>
#include <ktl/array.h>

class Scheduler;
extern fbl::NullLock chainlock_transaction_token;

namespace kconcurrent {

// fwd decls
class ChainLockTransaction;
class TA_CAP("mutex") ChainLock : protected ::concurrent::ChainLock {
 private:
  using Base = ::concurrent::ChainLock;

 public:
  using Base::LockResult;
  using Base::Token;

  constexpr ChainLock() = default;
  ~ChainLock() = default;

  // No copy, no move
  ChainLock(const ChainLock&) = delete;
  ChainLock(ChainLock&&) = delete;
  ChainLock& operator=(const ChainLock&) = delete;
  ChainLock& operator=(ChainLock&&) = delete;

  // Acquire and Release routines which need to bounce into an un-annotated
  // "internal" version.
  //
  // Note: this unfortunate style comes down to a quirk in the behavior of clang
  // static lock analysis.  When a method of a capability-class is annotated
  // with ACQUIRE, TRY_ACQUIRE, or RELEASE, the analyzer basically takes it on
  // faith that the internals of the class simply "does what it takes" to
  // implement the acquire/release functionality.  The implementation of the
  // method is effectively _TA_NO_THREAD_SAFETY_ANALYSIS on the inside.
  //
  // Were we simply adding some pre/post-condition behavior to our base
  // ChainLock, this would be fine, but unfortunately we are going a bit
  // farther.  In particular, bookkeeping interactions with the global
  // ChainLockTransaction state also wants to be able to enforce using static
  // analysis tools.  This issue seems to crop up frequently when attempting to
  // apply Clang static analysis to implementations of actual annotated
  // synchronization objects in the kernel which depending on even lower level
  // synchronization objects and statically enforced preconditions, see the
  // implementation of the kernel's Mutex for examples.
  //
  // As a workaround, for any method which has any of these annotations, we
  // simply re-direct our implementation to an un-annotated internal version of
  // the same routine which is assumed by the non-annotated public facing
  // version of the route which is assumed to do the job correctly.  IMPORTANT -
  // Any static preconditions which are required by the internal implementation,
  // and which are established by the public version of the method, **must** be
  // either statically annotated or dynamically established in the public-facing
  // version of the method before calling the internal version.  The
  // static-analyzer will not catch any mistakes, please be extra careful.
  //
  void AcquireUnconditionally() TA_REQ(chainlock_transaction_token) TA_ACQ() {
    AcquireUnconditionallyInternal();
  }
  bool TryAcquire() TA_REQ(chainlock_transaction_token) TA_TRY_ACQ(true) {
    return TryAcquireInternal();
  }
  void Release() TA_REQ(chainlock_transaction_token) TA_REL() { ReleaseInternal(); }
  void AssertAcquired() const TA_REQ(chainlock_transaction_token) TA_ACQ() {
    AssertAcquiredInternal();
  }

  // A helper used to work around some uncommon sticky situations we can run
  // into with the static analyzer.  `MarkNeedsReleaseIfHeld` will test a
  // ChainLock to see if it is currently held by the running thread, and if so
  // it will return true "marking" the lock capability as being acquired from
  // the static analyzer's perspective, forcing us to release it before we exit
  // the scope.  Otherwise, it returns false and the analyzer assumes nothing.
  //
  // One example of where this can be helpful is when we encounter a backoff error
  // while attempting to acquire the locks for the set of threads blocked in a
  // wait queue.  We needed to release all of the locks held along the path
  // before unwinding, but the analyzer does not know that we are holding the
  // locks of some of the members of the wait queue.  Generally speaking, there
  // is no good way to statically know that we hold an arbitrary set of locks,
  // all of the same class.
  //
  // Even if we know specifically which locks are held, we cannot simply release
  // them since the analyzer thinks that we don't hold them.  Instead, we can
  // use `MarkNeedsReleaseIfHeld`.  While the method is annotated with a
  // try_acquire annotation, it does not _actually_ acquire the lock.  Instead,
  // it just tests to see if the lock is held, and the try_acquire annotation
  // will tell the analyzer both that we are currently holding the lock *and*
  // that it is our responsibility to release the lock before we exit the scope
  // (making the call to Release legal).
  bool MarkNeedsReleaseIfHeld() const TA_REQ(chainlock_transaction_token) TA_TRY_ACQ(true) {
    return MarkNeedsReleaseIfHeldInternal();
  }

  // Routines which do not need an internal implementation as they take no
  // direct stance on ACQUIRE/TRY_ACQUIRE/RELEASE annotations.
  LockResult Acquire() TA_REQ(chainlock_transaction_token);
  void AssertHeld() const TA_REQ(chainlock_transaction_token) TA_ASSERT();
  bool is_held() const TA_REQ(chainlock_transaction_token);

  // "Mark" routines, used to work around situations where the static analyzer
  // has trouble following what is going on.
  //
  // Occasionally, we end up in a situation with the static analyzer where it is
  // difficult, or even impossible, to annotate functions in a way where the
  // static analyzer has the correct notion of what the actual lock-state of the
  // system is.
  //
  // Most of the time, this happens in situations where we are using a type
  // erased lambda function (think; capturing a lambda for `fit::defer` or a
  // functional pattern such as `collection.foreach`), but it can show up other
  // places as well (such as during context switches to a newly starting
  // thread's entry point).
  //
  // *Most* of the time, the preferred solution to something like this is is to
  // use annotations such as `AssertHeld` (see above), which will check at
  // runtime to make sure that we actually hold the capability, and DEBUG_ASSERT
  // if we don't.  The TA_ASSERT annotation tells the analyzer that we have
  // checked (at runtime) and we are certain that we hold the capability.
  //
  // Sometimes, however, we don't really want to pay the cost of an actual
  // runtime check, because it is trivially obvious from the context that we do
  // hold the proper locks.  For example;
  //
  // ```
  // foo.get_lock().AcquireUnconditionally();
  // collection.foreach([&foo]() {
  //   foo.get_lock().MarkHeld();
  //   DoThingWhichRequiresFooLock();
  // });
  // ```
  //
  // We _could_ assert held and add a runtime check, but we really should not
  // need to.  Some other examples include when we have just runtime checked to
  // verify that an acquire succeeded (but cannot use the TRY_ACQUIRE annotation
  // because of static-analyzer limitations), and during some situations
  // involving dynamic downcases.  See the ulib/concurrent headers for more
  // examples.
  //
  // Enter the "mark" routines.  Each of these carries an annotation which tells
  // the static analyzer what is going on, but none of them actually _do_
  // anything.  No runtime checks are performed, and no lock state is ever
  // changed.
  //
  // + MarkHeld tells the analyzer that we hold the lock, but does not tell the
  //   analyzer that it is our responsibility to release it.
  // + MarkNeedsRelease not only tells the analyzer that we hold the lock, but
  //   also that it is our responsibility to release the lock before we exit the
  //   scope where the Mark method was invoked.
  // + MarkReleased tells the analyzer that a lock has been released, in the
  //   rare case that the analyzer thinks we are holding the lock, but it has
  //   actually already been released.
  //
  // Best practice for using these tools is:
  // 1) Prefer to use one of these tools as opposed to disabling analysis
  //    entirely.  Disabling analysis has the unfortunate side effect of
  //    disabling _all_ analysis, not just the analysis for one specific
  //    unfortunate misunderstanding.
  // 2) Only use these routines when there is either no other options (aside
  //    from the top level "disable") or where the state being marked is 100%
  //    clear and obvious from the code immediately surrounding the usage.  So,
  //    if you cannot see the code which caused a lock to become held on the
  //    same screen as the point where you need to convince the analyser that
  //    the lock _is_ held, you should probably be using `AssertHeld` instead of
  //    `MarkHeld`.
  //
  void MarkHeld() const TA_REQ(chainlock_transaction_token) TA_ASSERT() { return Base::MarkHeld(); }
  void MarkNeedsRelease() const TA_REQ(chainlock_transaction_token) TA_ACQ() {
    return Base::MarkNeedsRelease();
  }
  void MarkReleased() const TA_REQ(chainlock_transaction_token) TA_REL() {
    return Base::MarkReleased();
  }

 private:
  friend class ChainLockTransaction;

  void AcquireUnconditionallyInternal() TA_REQ(chainlock_transaction_token)
      TA_ACQ(static_cast<Base*>(this));
  bool TryAcquireInternal() TA_REQ(chainlock_transaction_token)
      TA_TRY_ACQ(true, static_cast<Base*>(this));
  void ReleaseInternal() TA_REQ(chainlock_transaction_token) TA_REL(static_cast<Base*>(this));
  void AssertAcquiredInternal() const TA_REQ(chainlock_transaction_token)
      TA_ACQ(static_cast<const Base*>(this));
  bool MarkNeedsReleaseIfHeldInternal() const TA_REQ(chainlock_transaction_token)
      TA_TRY_ACQ(true, static_cast<const Base*>(this));

  // Make single attempt to acquire the chain lock, and record the start of a
  // conflict in the active CLT if we fail for any reason. Returns
  // LockResult::kOk if the lock was successfully obtained.
  template <typename = void>
  inline LockResult AcquireInternalSingleAttempt(ChainLockTransaction& clt)
      TA_REQ(chainlock_transaction_token);

  // Special methods used only in the core of the scheduler to handle some edge
  // cases during context switching.
  static Token CreateSchedToken() { return Base::CreateToken(arch_curr_cpu_num() + 1); }

  static void AssertTokenIsSchedToken(const ChainLock::Token token) {
    DEBUG_ASSERT_MSG(token.is_reserved(), "val %lu", token.value());
  }

  static void AssertTokenIsNotSchedToken(const ChainLock::Token token) {
    DEBUG_ASSERT_MSG(token.is_valid() && !token.is_reserved(), "val %lu", token.value());
  }

  static void ReplaceToken(ChainLock::Token& old_token, ChainLock::Token new_token) {
    Base::ReplaceToken(old_token, new_token);
  }

  void ReplaceLockToken(const ChainLock::Token token) TA_REQ(*this) {
    state_.store(token, ktl::memory_order_release);
  }
};

}  // namespace kconcurrent

using ChainLock = ::kconcurrent::ChainLock;

class TA_SCOPED_CAP UnconditionalChainLockGuard {
 public:
  // TODO(johngro):
  // Properly annotate this with TA_REQ(chainlock_transaction_token) if/when
  // https://github.com/llvm/llvm-project/issues/65127 is ever resolved.
  //
  // We cannot properly annotate this guard as requiring that we hold the
  // "chainlock_transaction_token" capability at this point in time, even though
  // we absolutely do need to be in a CLT before any attempt to obtain a
  // ChainLock.
  //
  // The main issue here seems to be that if we TA_REQ(a) and TA_ACQ(b) during
  // the construction of the scoped capability, Clang's analyzer incorrectly
  // thinks that _both_ |a| and |b| are released when methods with the TA_REL
  // annotation of the scoped capability are release, instead of just those
  // which were acquired.
  __WARN_UNUSED_CONSTRUCTOR explicit UnconditionalChainLockGuard(ChainLock& lock) TA_ACQ(lock)
      : lock_(&lock) {
    lock_->AcquireUnconditionally();
  }

  ~UnconditionalChainLockGuard() TA_REQ(chainlock_transaction_token) TA_REL() { Release(); }

  void Release() TA_REL() {
    if (lock_ != nullptr) {
      lock_->Release();
      lock_ = nullptr;
    }
  }

 private:
  ChainLock* lock_;
};

// Attempt to acquire all of the locks in the given chain lock set.  If an error
// is encountered, unlock any locks which had been obtained and return the
// error.
template <size_t N>
ChainLock::LockResult AcquireChainLockSet(const ktl::array<ChainLock*, N>& lock_set)
    TA_REQ(chainlock_transaction_token) {
  for (size_t lock_ndx = 0; lock_ndx < lock_set.size(); ++lock_ndx) {
    const ChainLock::LockResult res = lock_set[lock_ndx]->Acquire();

    if (res != ChainLock::LockResult::kOk) {
      for (size_t unlock_ndx = 0; unlock_ndx < lock_ndx; ++unlock_ndx) {
        lock_set[unlock_ndx]->AssertAcquired();
        lock_set[unlock_ndx]->Release();
      }
      return res;
    }
  }
  return ChainLock::LockResult::kOk;
}

#endif  // ZIRCON_KERNEL_LIB_KCONCURRENT_INCLUDE_LIB_KCONCURRENT_CHAINLOCK_H_
