// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_KCONCURRENT_INCLUDE_LIB_KCONCURRENT_CHAINLOCK_TRANSACTION_H_
#define ZIRCON_KERNEL_LIB_KCONCURRENT_INCLUDE_LIB_KCONCURRENT_CHAINLOCK_TRANSACTION_H_

#include <assert.h>
#include <lib/fxt/interned_string.h>
#include <lib/kconcurrent/chainlock.h>
#include <lib/ktrace.h>
#include <stdint.h>

#include <arch/arch_interrupt.h>
#include <kernel/percpu.h>
#include <kernel/spin_tracing_config.h>
#include <ktl/type_traits.h>

namespace kconcurrent {

// Produce trace records documenting the CLT contention overhead if we have lock
// contention tracing enabled.
static inline constexpr bool kCltTraceAccountingEnabled = kSchedulerLockSpinTracingEnabled;

struct SchedulerUtils;

namespace internal {
struct TrampolineTransactionTag {};

// Strictly speaking, ChainLockTransactions do not need to keep track of either
// the number of locks they hold, or their finalized/not-finalized state in
// order to operate.  That said, maintaining these fields allows us implement a
// bunch of runtime asserts which can be used to verify that all of the rules
// are being followed.
//
// So, we introduce a specialized storage struct which ChainLockTransactions can
// use to enable/disable both the storage overhead as well as all of the
// maintenance/assert overhead.
template <bool Enabled>
class CltDebugAccounting;

template <>
class CltDebugAccounting<false> {
 public:
  CltDebugAccounting() = default;
  ~CltDebugAccounting() = default;
  explicit CltDebugAccounting(TrampolineTransactionTag) {}

  CltDebugAccounting(const CltDebugAccounting&) = delete;
  CltDebugAccounting(CltDebugAccounting&&) = delete;
  CltDebugAccounting& operator=(const CltDebugAccounting&) = delete;
  CltDebugAccounting& operator=(CltDebugAccounting&&) = delete;

  void IncLocksHeld() {}
  void DecLocksHeld() {}
  void SetFinalized(bool) {}

  void AssertAtLeastOneLockHeld() const {}
  void AssertAtMostOneLockHeld() const {}
  void AssertNumLocksHeld(uint32_t) const {}
  void AssertFinalized() const {}
  void AssertNotFinalized() const {}

  // Notes about the odd template style used here.  We never want anyone to be
  // calling either locks_held or finalized in functional code.  They are really
  // only here for when someone needs to add some debug logging instrumentation,
  // and should never be used when debug accounting is disabled.
  //
  // We cannot simply statically assert false here, as that would always fire
  // (whether or not the method itself was ever used).  By adding a template
  // typename parameter, we force the compiler delay evaluation of the static
  // assert until after someone actually uses the method and defines T in the
  // process.  Note that T is defaulted, so they don't actually have to supply a
  // value, but no matter what they supply, the static assert will trivially
  // fail.
  template <typename T = void>
  uint32_t locks_held() const {
    static_assert(!ktl::is_same_v<T, T>,
                  "Cannot use locks_held() when debug accounting is disabled");
    return 0;
  }

  template <typename T = void>
  bool finalized() const {
    static_assert(!ktl::is_same_v<T, T>,
                  "Cannot use finalized() when debug accounting is disabled");
    return false;
  }
};

template <>
class CltDebugAccounting<true> {
 public:
  CltDebugAccounting() = default;
  ~CltDebugAccounting() = default;
  explicit CltDebugAccounting(TrampolineTransactionTag) : locks_held_{2}, finalized_{true} {}

  CltDebugAccounting(const CltDebugAccounting&) = delete;
  CltDebugAccounting(CltDebugAccounting&&) = delete;
  CltDebugAccounting& operator=(const CltDebugAccounting&) = delete;
  CltDebugAccounting& operator=(CltDebugAccounting&&) = delete;

  void IncLocksHeld() { ++locks_held_; }
  void DecLocksHeld() { --locks_held_; }
  void SetFinalized(bool state) { finalized_ = state; }

  void AssertAtLeastOneLockHeld() const { ASSERT(locks_held_ > 0); }
  void AssertAtMostOneLockHeld() const { ASSERT(locks_held_ <= 1); }
  void AssertNumLocksHeld(uint32_t expected) const {
    ASSERT_MSG(expected == locks_held_, "Expected %u locks to be held, but state reports %u locks",
               expected, locks_held_);
  }
  void AssertFinalized() const { ASSERT(finalized_ == true); }
  void AssertNotFinalized() const { ASSERT(finalized_ == false); }

  // See notes above.
  template <typename>
  uint32_t locks_held() const {
    return locks_held_;
  }
  template <typename>
  bool finalized() const {
    return finalized_;
  }

 private:
  uint32_t locks_held_{0};
  bool finalized_{false};
};

template <bool Enabled>
class CltTraceAccounting;

template <>
class CltTraceAccounting<false> {
 public:
  constexpr CltTraceAccounting(const fxt::InternedString& func, int line) {}
  constexpr const char* func() const { return "<disabled>"; }
  constexpr int line() const { return 0; }
  void Finalize() {}

  template <typename = void>
  bool has_active_conflict() const {
    static_assert(
        false, "It should be impossible to call CltTraceAccounting<false>:has_active_conflict()");
    return false;
  }

  template <typename = void>
  void RecordConflictStart() {
    static_assert(
        false, "It should be impossible to call CltTraceAccounting<false>:RecordConflictStart()");
  }
};

template <>
class CltTraceAccounting<true> {
 public:
  CltTraceAccounting(const fxt::InternedString& func, int line)
      : func_{func.string}, encoded_id_{EncodeId(func.GetId(), line)} {}

  const char* func() const { return func_; }
  int line() const { return static_cast<int>(encoded_id_ & 0xFFFFFFFF); }

  void Finalize() {
    if (has_active_conflict()) {
      FXT_EVENT_COMMON(true, ktrace_category_enabled, ktrace::EmitComplete, "kernel:sched",
                       "lock_spin"_intern, conflict_start_time_, ktrace_timestamp(),
                       TraceContext::Thread, ("lock_id", lock_id()),
                       ("lock_class", fxt::StringRef<fxt::RefType::kId>{lock_class()}),
                       ("lock_type", "ChainLockTransaction"_intern));
    }
  }

  // Note that has_active_conflict and RecordConflictStart are methods which are
  // deliberately omitted from the "disabled" version CltTraceAccounting.  If
  // trace accounting is disabled, these methods should never be called, and the
  // suppression of these calls is something which should be controlled at
  // compile time.  Mechanisms are already in place to ensure that no mistakes
  // are made here, omitting the methods from the disabled version
  // CltTraceAccounting is just a bit of extra compile-time defense-in-depth.
  bool has_active_conflict() const { return conflict_start_time_ != kInvalidConflictStartTime; }

  void RecordConflictStart() {
    DEBUG_ASSERT(!has_active_conflict());
    conflict_start_time_ = current_ticks();
  }

 private:
  static inline constexpr zx_ticks_t kInvalidConflictStartTime = 0;
  static inline constexpr uint64_t EncodeId(uint16_t func_id, int line_id) {
    return (static_cast<uint64_t>(func_id) << 32) | static_cast<uint64_t>(line_id);
  }

  inline uint16_t lock_class() const { return static_cast<uint16_t>((encoded_id_ >> 32) & 0xFFFF); }
  inline uint32_t lock_id() const { return static_cast<uint32_t>(encoded_id_ & 0xFFFFFFFF); }

  const char* func_;
  uint64_t encoded_id_;
  zx_ticks_t conflict_start_time_{kInvalidConflictStartTime};
};
}  // namespace internal

#define CLT_TAG_EXPLICIT_LINE(tag, line)                                                           \
  []() -> ::kconcurrent::internal::CltTraceAccounting<::kconcurrent::kCltTraceAccountingEnabled> { \
    using fxt::operator"" _intern;                                                                 \
    return ::kconcurrent::internal::CltTraceAccounting<::kconcurrent::kCltTraceAccountingEnabled>{ \
        tag##_intern, line};                                                                       \
  }()

#define CLT_TAG(tag) CLT_TAG_EXPLICIT_LINE(tag, __LINE__)

// Please refer to the "ChainLockTransaction" section of
// `//zircon/kernel/lib/kconcurrent/docs/chainlock.md` for a description of what
// a ChainLockTransaction is, what it does, and how to use it in kernel code
// involving `ChainLock`s
class ChainLockTransaction {
 private:
  static inline constexpr bool kDebugAccountingEnabled = DEBUG_ASSERT_IMPLEMENTED;

 public:
  using CltTraceAccounting =
      ::kconcurrent::internal::CltTraceAccounting<kCltTraceAccountingEnabled>;

  // Static tests and asserts to check if, or prove that, there is an active
  // ChainLock transaction.  Notes:
  //
  // 1) Active will return nullptr if there is no currently active ChainLockTransaction.
  // 2) It is an error to call ActiveRef if there is no currently active
  //    ChainLockTransaction, which is why it has been statically annotated to
  //    require the chainlock_transaction_token (which should provide a compile
  //    time guarantee that there is an active transaction).
  //
  static ChainLockTransaction* Active() { return arch_get_curr_percpu()->active_cl_transaction; }
  static ChainLockTransaction& ActiveRef() TA_REQ(chainlock_transaction_token) {
    ChainLockTransaction* const transaction = Active();
    DEBUG_ASSERT(transaction != nullptr);
    return *transaction;
  }

  static void AssertActive() TA_ASSERT(chainlock_transaction_token) {
    DEBUG_ASSERT(arch_ints_disabled());
    DEBUG_ASSERT(nullptr != Active());
  }

  // Tell the static analyzer that we are in the middle of an active
  // ChainLockTransaction, but don't perform any runtime checks or ASSERTs.
  // Like other `Mark` style static analyzer helpers, this should only be used
  // in places where it already 100% clear from immediate context that we are in
  // a CLT, we just need the analyzer to understand that as well.
  static void MarkActive() TA_ASSERT(chainlock_transaction_token) {}

  // Asserts involving the state of a transaction's extra accounting (when
  // enabled).
  void AssertAtLeastOneLockHeld() const { debug_.AssertAtLeastOneLockHeld(); }
  void AssertAtMostOneLockHeld() const { debug_.AssertAtMostOneLockHeld(); }
  void AssertNumLocksHeld(uint32_t expected) const { debug_.AssertNumLocksHeld(expected); }
  void AssertFinalized() const { debug_.AssertFinalized(); }
  void AssertNotFinalized() const { debug_.AssertNotFinalized(); }

  // Transition an active chainlock transaction to a finalized chainlock
  // transaction.  Once the transaction has been finalized, no new locks may be
  // obtained, only dropped.
  void Finalize() TA_REQ(chainlock_transaction_token) {
    debug_.AssertNotFinalized();
    debug_.SetFinalized(true);
    trace_.Finalize();
  }

  // Restart a transaction which has already been finalized.  This method is
  // similar to destructing and re-constructing the transaction in place.  It
  // requires that at most one lock is being held, and that the transaction was
  // finalized.
  void Restart(const CltTraceAccounting& trace_accounting) TA_REQ(chainlock_transaction_token) {
    debug_.AssertFinalized();
    debug_.AssertAtMostOneLockHeld();
    DEBUG_ASSERT(active_token_.is_valid());
    debug_.SetFinalized(false);
    trace_ = trace_accounting;
  }

  ChainLock::Token token() const TA_REQ(chainlock_transaction_token) { return active_token_; }

  // Note that these methods are for debug logging only, no load-bearing code
  // should ever depend on them existing (the bookkeeping does not exist in
  // release builds).  See the notes in the CltDebugAccounting class for how we
  // abuse templates in an attempt to enforce this rule.
  template <typename T = void>
  uint32_t locks_held() const TA_REQ(chainlock_transaction_token) {
    return debug_.locks_held<T>();
  }
  template <typename T = void>
  bool is_finalized() const TA_REQ(chainlock_transaction_token) {
    return debug_.finalized<T>();
  }

  // Relax has to be implemented by the specific type of CLT (NoIrqSave,
  // IrqSave, etc)
  //
  // TODO(johngro): should we be doing this with vtables, or would it be better
  // to store a type and switch our behavior based on that.
  virtual void Relax() = 0;

 protected:
  friend class ::kconcurrent::ChainLock;
  friend struct ::kconcurrent::SchedulerUtils;

  // ChainLockTransaction objects should not be instantiated directly.  Instead, users should
  // instantiate transactions using one of the specialized versions.
  //
  // + ChainLockTransactionNoIrqSave
  // + ChainLockTransactionIrqSave
  // + ChainLockTransactionPreemptDisableAndIrqSave
  // + ChainLockTransactionEagerReschedDisableAndIrqSave
  //
  explicit ChainLockTransaction(CltTraceAccounting trace_accounting) : trace_(trace_accounting) {}

  // Note: while it is an odd pattern, we don't actually mark the CLT destructor
  // as virtual (even though we have a single pure virtual function; Relax).
  //
  // ChainLockTransactions should _never_ destruct directly, but only when one
  // of its explicit subclasses goes out of scope.  One of the reasons that the
  // base class destructor is protected is to prevent this from ever happening.
  ~ChainLockTransaction() = default;

  // Construct the CLT used during thread trampolines.  This is the CLT which
  // needs to be "restored" as we context switch from an old thread to this new
  // thread for the first time.  It should appear to be using the specialized
  // scheduler-token for this CPU, hold two locks (the previous thread's and the
  // current thread's), and should have been finalized.
  //
  // It should _not_ attempt to register itself.  We already have a CLT
  // registered (the previous thread's) and we are about to swap it out.
  //
  explicit ChainLockTransaction(internal::TrampolineTransactionTag t)
      : debug_(t), trace_{CLT_TAG_EXPLICIT_LINE("Trampoline", 0)} {}

  // No copy no move
  ChainLockTransaction(const ChainLockTransaction&) = delete;
  ChainLockTransaction& operator=(const ChainLockTransaction&) = delete;
  ChainLockTransaction(ChainLockTransaction&&) = delete;
  ChainLockTransaction& operator=(ChainLockTransaction&&) = delete;

  static void SetActive(ChainLockTransaction* transaction) {
    arch_get_curr_percpu()->active_cl_transaction = transaction;
  }

  // Unfortunately, these cannot currently be inlined because of header
  // dependency order.
  //
  // TODO(johngro): Look into breaking the dependency between
  // chainlock_transaciton.h and thread.h so we can just call
  // Thread::Current::Preempt(Disable|Enable) instead.
  static void PreemptDisable();
  static void PreemptReenable();
  static void EagerReschedDisable();
  static void EagerReschedReenable();

  // Registers this ChainLockTransaction as the currently active CLT with the
  // current CPU. Interrupts must already be disabled (typically by the IrqSave
  // version of the CLT), and the CPU must not have a currently active chain
  // lock transaction.
  void Register() {
    DEBUG_ASSERT(arch_ints_disabled());
    DEBUG_ASSERT_MSG(const ChainLockTransaction* const active = Active();
                     active == nullptr,
                     "Started a ChainLockTransaction with one already active "
                     "(ptr %p, name '%s:%d')",
                     active, active->trace_.func(), active->trace_.line());
    SetActive(this);
  }

  // Removes this ChainLockTransaction instance as the currently active CLT
  // registered with the current CPU. Interrupts must still be disabled
  // (typically to be released by the IrqSave version of the CLT immediately
  // after it calls unregister), and |this| must be the same as the CPU's
  // currently active transaction.
  void Unregister() {
    DEBUG_ASSERT(arch_ints_disabled());
    DEBUG_ASSERT_MSG(const ChainLockTransaction* const active = Active();
                     active == this,
                     "Ending a ChainLockTransaction with a different one already active "
                     "(this %p,'%s:%d', active %p,'%s:%d')",
                     this, this->trace_.func(), this->trace_.line(), active, active->trace_.func(),
                     active->trace_.line());
    SetActive(nullptr);
  }

  // Accessors used directly by our ChainLock friend.
  const ChainLock::Token& active_token() const TA_REQ(chainlock_transaction_token) {
    return active_token_;
  }
  void OnAcquire() TA_REQ(chainlock_transaction_token) { debug_.IncLocksHeld(); }
  void OnRelease() TA_REQ(chainlock_transaction_token) {
    debug_.AssertAtLeastOneLockHeld();
    debug_.DecLocksHeld();
  }

  // Hooks used for contention overhead tracing.  These should never be called
  // unless kCltTraceAccountingEnabled is enabled.  The funny template syntax
  // used here is what allows us to successfully statically assert that this is
  // the case, by forcing the compiler to wait until an attempt is actually made
  // to use the method before evaluating the static_assert.
  template <typename = void>
  bool has_active_conflict() const {
    static_assert(kCltTraceAccountingEnabled,
                  "Calls to has_active_conflict may only be made when ChainLock trace accounting "
                  "is enabled.");
    return trace_.has_active_conflict();
  }

  template <typename = void>
  void RecordConflictStart() {
    static_assert(kCltTraceAccountingEnabled,
                  "Calls to RecordConflictStart may only be made when ChainLock trace accounting "
                  "is enabled.");
    trace_.RecordConflictStart();
  }

  // The token used to obtain all ChainLocks during this transaction.
  ChainLock::Token active_token_{};

  // The per-cpu conflict ID recorded the last time that a thread encountered a
  // conflict and was forced to back off during this transaction.  When threads
  // successfully record a conflict, they can back off to the Relax point, and
  // wait for their CPU's conflict ID (stored in the per-cpu data structure) to
  // change.  This will happen when the thread who currently owns the lock which
  // triggered the backoff releases the lock, letting the Relaxing thread know
  // that it may now make another attempt.
  uint64_t conflict_id_{0};

  // Extra data used to support more internal consistency checks.  Only present
  // when enabled.
  [[no_unique_address]] internal::CltDebugAccounting<kDebugAccountingEnabled> debug_{};

  // Extra data used to support conflict tracing.  Only present when enabled.
  [[no_unique_address]] internal::CltTraceAccounting<kCltTraceAccountingEnabled> trace_;
};

enum class CltType : uint32_t {
  NoIrqSave,
  IrqSave,
  PreemptDisable,
  EagerReschedDisable,
};

namespace internal {

template <CltType kType, typename = void>
struct CltState {};

template <CltType kType>
struct CltState<kType, ktl::enable_if_t<kType != CltType::NoIrqSave>> {
  interrupt_saved_state_t interrupt_state;
};

template <CltType kType>
class TA_SCOPED_CAP ChainLockTransaction : public ::kconcurrent::ChainLockTransaction {
 private:
  using Base = ::kconcurrent::ChainLockTransaction;

 public:
  explicit ChainLockTransaction(CltTraceAccounting trace_accounting)
      TA_ACQ(chainlock_transaction_token)
      : Base{trace_accounting} {
    DoRegister();
  }
  ~ChainLockTransaction() TA_REL(chainlock_transaction_token) { DoUnregister(); }

  // No copy, no move.
  ChainLockTransaction(const ChainLockTransaction&) = delete;
  ChainLockTransaction& operator=(const ChainLockTransaction&) = delete;
  ChainLockTransaction(ChainLockTransaction&&) = delete;
  ChainLockTransaction&& operator=(ChainLockTransaction&&) = delete;

  // Used by clients in a lock-acquisition retry loop when they are restarting
  // their attempt to acquire their set of locks.  Clients must have already
  // dropped all locks, and the transaction cannot have been finalized yet.
  //
  // Please refer to the "Implementing proper Relax behavior" section of
  // `//zircon/kernel/lib/kconcurrent/docs/chainlock.md` for more details.
  void Relax() final TA_REQ(chainlock_transaction_token) {
    debug_.AssertNumLocksHeld(0);
    debug_.AssertNotFinalized();

    auto pause = [this](const struct percpu& pcpu) {
      if (conflict_id_) {
        do {
          arch::Yield();
        } while (pcpu.chain_lock_conflict_id.load(ktl::memory_order_acquire) == conflict_id_);
        conflict_id_ = 0;
      } else {
        arch::Yield();
      }
    };

    // We might be about to either re-enable preemption or interrupt (or both)
    // depending on what their states were when we entered this CLT.
    //
    // If there is any chance we might become interrupted (either via a pending
    // preempt or an pending IRQ), we need to make sure that our CLT has been
    // unregistered so a new one can be started during the preemption/irq event.
    //
    // TODO(johngro): It may be worthwhile to optimize this.  If preemption/irqs
    // were already disabled when we entered this transaction, then there is no
    // point in either attempting to restore/re-save the state, or
    // unregister/register the transaction.
    // un-register if interrupts were already disabled when this IrqSave version
    // of the CLT was instantiated.  If we added (to the arch level) the ability
    // to check the saved state to see if IRQs were already disabled when we
    // entered, we can skip the un-register/re-register steps.
    const struct percpu& pcpu = *arch_get_curr_percpu();
    if constexpr (kType != CltType::NoIrqSave) {
      DoUnregister();
      pause(pcpu);
      DoRegister();
    } else {
      pause(pcpu);
    }
  }

 private:
  friend struct ::kconcurrent::SchedulerUtils;

  explicit ChainLockTransaction(internal::TrampolineTransactionTag t)
      TA_REQ(chainlock_transaction_token)
      : Base(t) {
    static_assert(kType == CltType::NoIrqSave);
  }

  void DoRegister() {
    if constexpr (kType == CltType::EagerReschedDisable) {
      EagerReschedDisable();
    } else if constexpr (kType == CltType::PreemptDisable) {
      PreemptDisable();
    }

    if constexpr (kType != CltType::NoIrqSave) {
      state_.interrupt_state = arch_interrupt_save();
    }

    Register();
  }

  void DoUnregister() {
    Unregister();

    if constexpr (kType != CltType::NoIrqSave) {
      arch_interrupt_restore(state_.interrupt_state);
    }

    if constexpr (kType == CltType::EagerReschedDisable) {
      EagerReschedReenable();
    } else if constexpr (kType == CltType::PreemptDisable) {
      PreemptReenable();
    }
  }

  CltState<kType> state_;
};

}  // namespace internal

using ChainLockTransactionNoIrqSave = internal::ChainLockTransaction<CltType::NoIrqSave>;
using ChainLockTransactionIrqSave = internal::ChainLockTransaction<CltType::IrqSave>;

class TA_SCOPED_CAP ChainLockTransactionPreemptDisableAndIrqSave
    : public internal::ChainLockTransaction<CltType::PreemptDisable> {
 private:
  using Base = internal::ChainLockTransaction<CltType::PreemptDisable>;

 public:
  ChainLockTransactionPreemptDisableAndIrqSave(
      ChainLockTransaction::CltTraceAccounting trace_accounting)
      TA_ACQ(chainlock_transaction_token, preempt_disabled_token)
      : Base(trace_accounting) {}
  ~ChainLockTransactionPreemptDisableAndIrqSave()
      TA_REL(chainlock_transaction_token, preempt_disabled_token) {}
};

class TA_SCOPED_CAP ChainLockTransactionEagerReschedDisableAndIrqSave
    : public internal::ChainLockTransaction<CltType::EagerReschedDisable> {
 private:
  using Base = internal::ChainLockTransaction<CltType::EagerReschedDisable>;

 public:
  ChainLockTransactionEagerReschedDisableAndIrqSave(
      ChainLockTransaction::CltTraceAccounting trace_accounting)
      TA_ACQ(chainlock_transaction_token, preempt_disabled_token)
      : Base(trace_accounting) {}
  ~ChainLockTransactionEagerReschedDisableAndIrqSave()
      TA_REL(chainlock_transaction_token, preempt_disabled_token) {}
};

struct RescheduleContext {
 private:
  friend struct ::kconcurrent::SchedulerUtils;
  friend class ::Scheduler;

  RescheduleContext(ChainLockTransaction* clt, const ChainLock::Token token)
      : orig_transaction{clt}, orig_token{token} {}

  ChainLockTransaction* const orig_transaction;
  const ChainLock::Token orig_token;
};

// The scheduler has to do a few very special things in order to properly
// reschedule and context switch.  We don't really want anyone else to be doing
// stuff like this, and we don't really want to just give access to all of the
// internals of ChainLocks and ChainLockTransactions.
//
// Instead, we introduce |SchedulerUtils|.  It contains all of the specialized
// operations used during rescheduling as static private methods.  It declares
// the Scheduler as a friend, and is declared as a friend of the ChainLock and
// ChainLockTransaction classes.  This allows the Scheduler (an no one else) to
// perform its specialized actions, while not simply handing over total access
// to the lock internals.  All of the Very Special Scheduler Stuff should be
// localized to just these methods.
//
struct SchedulerUtils {
 private:
  friend class ::Scheduler;

  // At the start of a reschedule operation, we should have an active chain lock
  // transaction and should be holding exactly one lock (that of the current
  // thread).
  //
  // We need to switch to using a special scheduler token to prevent schedulers
  // from ever losing lock arbitration and being told to back off.  Later on,
  // after the reschedule operation has completed, we will restore the original
  // token (after perhaps switching to a different CLT instance after a context
  // switch) before unwinding.
  //
  // Prepare for reschedule will:
  //
  // 1) Verify the pre-requisites stated above.
  // 2) Replace the active CLT's token with the scheduler token.
  // 3) Replace the current thread's lock's token with the scheduler token.
  // 4) Return the token which had been used so that it can be stored on the
  //    current thread's stack, and restored later on after the thread is
  //    scheduled to run again.
  //
  static RescheduleContext PrepareForReschedule() TA_REQ(chainlock_transaction_token);

  // The inverse of PrepareForReschedule, called when no context switch actually
  // took place.  This method will restore the original token which was used as
  // the start of RescheduleCommon in both the active transaction as well as
  // current thread's lock.  The actual active transaction pointer should not
  // have changed and should still be located on the active thread's stack.
  //
  static void RestoreRescheduleContext(const RescheduleContext& ctx)
      TA_REQ(chainlock_transaction_token);

  // Similar to RestoreRescheduleContext, but used after a context switch has
  // occurred.  Both the previous thread, and the current thread's locks should
  // be held at this point in time.  The current active transaction according to
  // per-cpu bookkeeping is the previous thread's transaction.  It will be
  // replaced by the (new) current thread's transaction, and the original token
  // used by the current thread will be restored to its transaction as well as
  // its lock.  Finally, the previous thread's lock will be dropped (but only
  // after it's transaction has been swapped out).
  static void PostContextSwitchLockHandoff(const RescheduleContext& ctx, Thread* previous_thread)
      TA_REQ(chainlock_transaction_token) TA_REL(previous_thread->get_lock());

  // A special-case method used to manage the lock handoff for a newly launched
  // thread.
  //
  // During normal operation, when a thread (A) enters the scheduler for a
  // reschedule operation, there must exist an active ChainLockTransaction
  // (A_trans, somewhere on the stack above the call) and current thread's lock
  // must be held.  When a new thread (B) is selected to run, it must first be locked
  // using A_trans.  Then, after the context switch takes place and we are
  // running on B's stack, we need to swap the active transaction (A_trans) for
  // the transaction that was on B's stack the last time it was switched away
  // from (B_trans).  Finally, we can drop A's lock and unwind with an active
  // transaction holding a single lock (B's lock), exactly as it was when B
  // entered the reschedule operation.
  //
  // Launching a thread for the first time, however, requires some cheating.
  // When B launches for the first time, it's entry point is the threads
  // trampoline routine.  It has no active transaction on the stack to switch
  // back to and it needs to synthesize one.
  //
  // TrampolineLockHandoff handles this special case.  It creates a special
  // "fake" transaction on its stack.  This transaction does not attempt to
  // automatically register itself as the new transaction (there is already an
  // active transaction), is created with its internal accounting saying it is
  // holding two locks (the previous thread's lock and the current thread's
  // lock).  Now the context switch can proceed as normal.  The new "fake"
  // B_trans is swapped in for the previous A_trans, the previous thread's lock
  // is dropped, and finally the current thread's lock is dropped.
  static void TrampolineLockHandoff();
};

template <typename TransactionType>
class TA_SCOPED_CAP SingletonChainLockGuard {
 public:
  SingletonChainLockGuard(ChainLock& lock,
                          ChainLockTransaction::CltTraceAccounting trace_accounting)
      TA_ACQ(chainlock_transaction_token, lock)
      : transaction{trace_accounting}, guard(lock) {
    transaction.Finalize();
  }
  ~SingletonChainLockGuard() TA_REL() {}

  SingletonChainLockGuard(const SingletonChainLockGuard&) = delete;
  SingletonChainLockGuard(SingletonChainLockGuard&&) = delete;
  SingletonChainLockGuard& operator=(const SingletonChainLockGuard&) = delete;
  SingletonChainLockGuard& operator=(SingletonChainLockGuard&&) = delete;

 private:
  // Note: order is important here.  The transaction must be listed first so
  // that it constructs first.
  TransactionType transaction;
  UnconditionalChainLockGuard guard;
};

using SingletonChainLockGuardNoIrqSave = SingletonChainLockGuard<ChainLockTransactionNoIrqSave>;
using SingletonChainLockGuardIrqSave = SingletonChainLockGuard<ChainLockTransactionIrqSave>;

}  // namespace kconcurrent

// Global namespace aliases to make using CLTs a bit easier.
using ChainLockTransaction = ::kconcurrent::ChainLockTransaction;
using ChainLockTransactionNoIrqSave = ::kconcurrent::ChainLockTransactionNoIrqSave;
using ChainLockTransactionIrqSave = ::kconcurrent::ChainLockTransactionIrqSave;
using ChainLockTransactionPreemptDisableAndIrqSave =
    ::kconcurrent::ChainLockTransactionPreemptDisableAndIrqSave;
using ChainLockTransactionEagerReschedDisableAndIrqSave =
    ::kconcurrent::ChainLockTransactionEagerReschedDisableAndIrqSave;

using SingletonChainLockGuardNoIrqSave = ::kconcurrent::SingletonChainLockGuardNoIrqSave;
using SingletonChainLockGuardIrqSave = ::kconcurrent::SingletonChainLockGuardIrqSave;

#endif  // ZIRCON_KERNEL_LIB_KCONCURRENT_INCLUDE_LIB_KCONCURRENT_CHAINLOCK_TRANSACTION_H_
