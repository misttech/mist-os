// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#ifndef ZIRCON_KERNEL_LIB_WAKE_VECTOR_INCLUDE_LIB_WAKE_VECTOR_H_
#define ZIRCON_KERNEL_LIB_WAKE_VECTOR_INCLUDE_LIB_WAKE_VECTOR_H_

#include <lib/relaxed_atomic.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <zircon/types.h>

#include <kernel/spinlock.h>
#include <ktl/array.h>
#include <ktl/forward.h>
#include <ktl/type_traits.h>

namespace wake_vector {

// Forward declaration.
class WakeEvent;

// WakeVector is an interface implemented by objects that will generate system wake events using the
// WakeEvent type. This interface provides diagnostic information about the wake vector to the
// suspend subsystem.
class WakeVector {
 public:
  // This constructor verifies that the derived class has a WakeEvent member at compile time to help
  // avoid misuse. A derived class must pass a pointer-to-member to its WakeEvent member. This
  // constructor does not touch the contents of the WakeEvent instance, which most likely is
  // uninitialized at this point.
  //
  // Example:
  //
  // MyWakeVector::MyWakeVector() : WakeVector{&MyWakeVector::wake_event_}, wake_event_{*this} {}
  //
  template <typename Class>
  explicit WakeVector(WakeEvent Class::* wake_event_member) {
    static_assert(ktl::is_base_of_v<WakeVector, Class>);
  }
  virtual ~WakeVector() = default;

  // Diagnostic information about the wake vector managed by the implementor of this interface.
  struct Diagnostics {
    // Indicates that the given wake vector is enabled and can generate wake events. Disabled wake
    // vectors are not listed in diagnostic logs.
    bool enabled = false;

    // The koid of the object implementing this interface, if any.
    zx_koid_t koid = ZX_KOID_INVALID;

    // Extra information specific to the wake vector that can aid in determining the source of the
    // wake event and potentially its state.
    ktl::array<char, ZX_MAX_NAME_LEN> extra{};

    // Utility to write into the extra field printf style.
    int PrintExtra(const char* format, ...) __PRINTFLIKE(2, 3) {
      va_list ap;
      va_start(ap, format);
      const int err = vsnprintf(extra.data(), extra.size(), format, ap);
      va_end(ap);
      return err;
    }
  };

  // Provides diagnostic information about the wake vector object implementing this interface.
  virtual void GetDiagnostics(Diagnostics& diagnostics_out) const = 0;
};

// The result of a request to wake up the system.
enum class WakeResult {
  // The system was not suspended at the time of the event.
  Active,

  // The system was suspended at the time of the wake event.
  Resumed,

  // The system was in the process of suspending at the time of the wake event.
  SuspendAborted,

  // An already pending wake event was triggered again.
  BadState,
};

// WakeEvent manages the lifecycle of wake events triggered by wake vectors.
//
// A system wake event may be triggered in response to an appropriately configured interrupt,
// exception, timer, or other future wake source that should resume the system from a suspended
// state. When a wake event is triggered, it enters the pending state and will prevent the system
// from entering suspend until it has been acknowledged. A pending wake event is automatically
// acknowledged when the WakeEvent instance is destroyed to prevent missing an acknowledgement that
// would render the system unable to suspend.
//
// WakeEvent maintains a global list of all instances for diagnostic purposes (i.e. logging wake
// events that pending before, during, and after suspend). WakeEvents are added to and removed from
// the global list using WakeEvent::Initialize and WakeEvent::Destroy, respectively. Because
// diagnostics access each WakeEvent, and its containing WakeVector, from the global list, care must
// be taken to avoid potential use-after-free hazards.
//
// Users of WakeEvents MUST adhere to the following rules:
// 1. A WakeEvent object MUST be instantiated as a member of a type that implements the WakeVector
//    interface, such that the lifetime of the containing type encoloses the lifetime of the
//    WakeEvent. DO NOT heap allocate WakeEvent instances separately from the referenced WakeVector.
// 2. The container of a WakeEvent instance SHOULD call WakeEvent::Initialize during construction /
//    initialization to register the WakeEvent on the global list. The container MAY skip the call
//    to WakeEvent::Initialize if initialization of the containing type fails.
// 3. The container of a WakeEvent MUST call WakeEvent::Destroy IFF WakeEvent::Initialize has been
//    called previously on the same instance of WakeEvent AND WakeEvent::Destroy MUST be called
//    BEFORE the containing type itself is destructed.
//
// WakeEvent::Destroy MAY be called in the destructor of the containing type, which will ensure that
// the WakeEvent is removed from the global list before any state in the containing type that
// diagnostics may access becomes invalid. However, extra care must be taken if a WakeEvent is a
// member of a base class and there are subclasses that override WakeVector::GetDiagnostics -- in
// these cases the subclass is responsible for calling WakeEvent::Destroy before its destructor
// completes INSTEAD of the base class to prevent use-after-free during races between diagnostics
// and the destruction of subclass state that diagnostics might access.
//
// AS A GENERAL RULE, it is safe to call WakeEvent::Initialize in a constructor and
// WakeEvent::Destroy in a destructor IF the destructor OR the implementation/override of
// WakeVector::GetDiagnostics can be marked final in the class making the calls.
//
// Calls to WakeEvent::Initialize and WakeEvent::Destroy must always be balanced. However, a
// WakeEvent MAY be initialized and destroyed more than once, as long as it is destroyed before its
// destructor is invoked.
//
class WakeEvent {
 public:
  // Construct a WakeEvent referencing the given wake_vector.
  explicit WakeEvent(const WakeVector& wake_vector) : wake_vector_(wake_vector) {}

  ~WakeEvent() {
    // Make sure this wake event no longer contributes to the pending wake event count of the
    // system.
    Acknowledge();

    // When the node_state_ member destructs is will assert !node_state_.InContainer(), ensuring
    // that calls to Initialize and Destroy are balanced.
  }

  // Adds this wake event to the global list. Asserts that it is not already on the list.
  void Initialize() {
    Guard<SpinLock, IrqSave> guard(WakeEventListLock::Get());
    WakeEvent::list_.push_back(this);  // Asserts !node_state_.InContainer().
  }

  // Removes this wake event from the global list. Asserts that it is on the list.
  void Destroy() {
    Guard<SpinLock, IrqSave> guard(WakeEventListLock::Get());
    WakeEvent::list_.erase(*this);  // Asserts node_state_.InContainer().
  }

  // Triggers a wakeup that resumes the system, or aborts an incomplete suspend sequence, and
  // prevents the system from starting a new suspend sequence.
  //
  // Must be called with interrupts and preempt disabled.
  //
  // Returns:
  //  - WakeResult::Active if this wake trigger occurred when the system was active.
  //  - WakeResult::Resumed if this or another wake trigger resumed the system.
  //  - WakeResult::SuspendAborted if this wake trigger occurred before suspend completed.
  //  - WakeResult::BadState if this wake event is already pending.
  //
  // Calls to |Trigger| and |Acknowledge| must be synchronized by the caller to guarantee that
  // updates are performed by a single actor at a time.
  //
  WakeResult Trigger();

  // Acknowledges a pending wake event, allowing the system to enter suspend when all other
  // suspend conditions are met.
  //
  // Calls to |Trigger| and |Acknowledge| must be synchronized by the caller to guarantee that
  // updates are performed by a single actor at a time.
  //
  void Acknowledge();

  // Walk the global list of all instances and dump diagnostic information to |f|. All events that
  // are currently pending OR that were triggered after the optional time value are logged.
  //
  // Safe to call concurrently with any and all methods, including ctors and dtors.
  static void Dump(FILE* f, zx_instant_boot_t log_triggered_after_boot_time = ZX_TIME_INFINITE);

  using NodeState = fbl::DoublyLinkedListNodeState<WakeEvent*>;
  struct NodeListTraits {
    static NodeState& node_state(WakeEvent& w) { return w.node_state_; }
  };

 private:
  using WakeEventList = fbl::DoublyLinkedListCustomTraits<WakeEvent*, WakeEvent::NodeListTraits>;

  DECLARE_SINGLETON_SPINLOCK(WakeEventListLock);
  inline static WakeEventList list_ TA_GUARDED(WakeEventListLock::Get());

  NodeState node_state_ TA_GUARDED(WakeEventListLock::Get());
  const WakeVector& wake_vector_;

  // Indicates whether this WakeEvent is pending and the last time it became pending.
  class PendingState {
   public:
    constexpr PendingState() = default;
    constexpr PendingState(bool pending, zx_instant_boot_ticks_t last_triggered_boot_ticks)
        : value_{PendingField(pending) | TicksField(last_triggered_boot_ticks)} {}

    PendingState(const PendingState&) = default;
    PendingState& operator=(const PendingState&) = default;

    constexpr bool pending() const { return value_ & kPendingBit; }

    constexpr zx_instant_boot_ticks_t last_triggered_boot_ticks() const {
      return static_cast<zx_instant_boot_ticks_t>(value_ & kTicksMask);
    }

    zx_instant_boot_t last_triggered_boot_time() const;

   private:
    static constexpr uint64_t kPendingBit = uint64_t{1} << 63;
    static constexpr uint64_t kTicksMask = kPendingBit - 1;

    static constexpr uint64_t PendingField(bool pending) { return pending ? kPendingBit : 0; }
    static constexpr uint64_t TicksField(zx_ticks_t ticks) {
      return static_cast<uint64_t>(ticks) & kTicksMask;
    }

    uint64_t value_{0};
  };
  static_assert(RelaxedAtomic<PendingState>::is_always_lock_free);

  // This is atomic because it may be accessed by a thread calling |Dump| in parallel with another
  // thread calling either |Trigger| or |Acknowledge|.
  RelaxedAtomic<PendingState> pending_state_{};
};

}  // namespace wake_vector

#endif  // ZIRCON_KERNEL_LIB_WAKE_VECTOR_INCLUDE_LIB_WAKE_VECTOR_H_
