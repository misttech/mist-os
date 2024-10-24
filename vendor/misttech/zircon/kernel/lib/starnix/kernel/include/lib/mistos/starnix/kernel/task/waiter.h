// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_WAITER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_WAITER_H_

#include <lib/mistos/starnix/kernel/lifecycle/atomic_counter.h>
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/starnix_uapi/vfs.h>
#include <lib/mistos/util/dense_map.h>
#include <lib/mistos/util/small_vector.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/interruptible_event.h>
#include <lib/starnix_sync/locks.h>
#include <lib/starnix_sync/port_event.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <limits>

#include <fbl/ref_counted.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/optional.h>
#include <ktl/variant.h>

namespace starnix {

using starnix_sync::InterruptibleEvent;
using starnix_sync::PortEvent;
using starnix_uapi::FdEvents;

class ReadyItemKey {
 public:
  using Variant = ktl::variant<FdNumber, size_t>;

  static ReadyItemKey FdNumber(FdNumber v) { return ReadyItemKey(ktl::move(v)); }
  static ReadyItemKey SizeT(size_t v) { return ReadyItemKey(v); }

 private:
  ReadyItemKey(Variant v) : key_(ktl::move(v)) {}

  Variant key_;
};

class ReadyItem {
 public:
  ReadyItem(const ReadyItemKey& key, const FdEvents& events) : key_(key), events_(events) {}

  ReadyItemKey& key() { return key_; }

  FdEvents& events() { return events_; }

 private:
  ReadyItemKey key_;
  FdEvents events_;
};

using FdEventsMapper = FdEvents (*)(FdEvents);

struct EnqueueEventHandler {
 public:
  ReadyItemKey key;
  fbl::RefPtr<starnix_sync::StarnixMutex<fbl::Vector<ReadyItem>>> queue;
  FdEvents sought_events;
  ktl::optional<FdEventsMapper> mappings;
};

struct Enqueue {
 public:
  EnqueueEventHandler handler;
};

struct EnqueueOnce {
 public:
  fbl::RefPtr<starnix_sync::StarnixMutex<ktl::optional<EnqueueEventHandler>>> handler;
};

class EventHandler {
 public:
  using Variant = ktl::variant<ktl::monostate, Enqueue, EnqueueOnce>;

  static EventHandler None() { return EventHandler(ktl::monostate{}); }
  static EventHandler EnqueueHandler(EnqueueEventHandler e) {
    return EventHandler(Enqueue{.handler = ktl::move(e)});
  }
  static EventHandler EnqueueOnceHandler(
      fbl::RefPtr<starnix_sync::StarnixMutex<ktl::optional<EnqueueEventHandler>>> e) {
    return EventHandler(EnqueueOnce{.handler = ktl::move(e)});
  }

  // Helpers from the reference documentation for ktl::visit<>, to allow
  // visit-by-overload of the ktl::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  // impl EventHandler
  void Handle(FdEvents events) const;

 private:
  explicit EventHandler(Variant handler) : handler_(ktl::move(handler)) {}

  Variant handler_;
};

struct ZxioSignalHandler {};

using ZxHandleSignalHandler = FdEvents (*)(zx_signals_t);

// The counter is incremented as each handle is signaled; when the counter reaches the handle
// count, the event handler is called with the given events.
struct ManyZxHandleSignalHandler {
  size_t count;
  // AtomicCounter<uint64_t> counter;
  zx_signals_t expected_signals;
  FdEvents events;
};

class SignalHandlerInner {
 public:
  using Variant = ktl::variant<ZxioSignalHandler, ZxHandleSignalHandler, ManyZxHandleSignalHandler>;

  static SignalHandlerInner FromZxioSignalHandler(ZxioSignalHandler e) {
    return SignalHandlerInner(ktl::move(e));
  }
  static SignalHandlerInner FromZxHandleSignalHandler(ZxHandleSignalHandler e) {
    return SignalHandlerInner(ktl::move(e));
  }
  static SignalHandlerInner ManyZxHandleSignalHandler(ManyZxHandleSignalHandler e) {
    return SignalHandlerInner(ktl::move(e));
  }

  // Helpers from the reference documentation for ktl::visit<>, to allow
  // visit-by-overload of the ktl::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  // impl SignalHandlerInner
 private:
  friend class SignalHandler;
  explicit SignalHandlerInner(Variant inner) : inner_(ktl::move(inner)) {}

  Variant inner_;
};

class SignalHandler {
 public:
  explicit SignalHandler(SignalHandlerInner inner, EventHandler event_handler)
      : inner_(ktl::move(inner)), event_handler_(ktl::move(event_handler)) {}

  // impl SignalHandler
  void Handle(zx_signals_t signals) const;

 private:
  SignalHandlerInner inner_;
  EventHandler event_handler_;
};

class WaitCallback {
 public:
  using Variant = ktl::variant<EventHandler, SignalHandler>;

  static WaitCallback EventHandlerCallback(EventHandler e) { return WaitCallback(ktl::move(e)); }
  static WaitCallback SignalHandlerCallback(SignalHandler e) { return WaitCallback(ktl::move(e)); }

  const Variant& callback() const { return callback_; }

  // Helpers from the reference documentation for ktl::visit<>, to allow
  // visit-by-overload of the ktl::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

 private:
  explicit WaitCallback(Variant callback) : callback_(ktl::move(callback)) {}

  Variant callback_;
};

struct WaitKey {
  uint64_t id;

  bool operator<(const WaitKey& other) const { return id < other.id; }
};

/// The different type of event that can be waited on / triggered.
class WaitEvents {
 public:
  using Variant = ktl::variant<ktl::monostate, FdEvents, uint64_t>;

  /// All event: a wait on `All` will be woken up by all event, and a trigger on `All` will wake
  /// every waiter.
  static WaitEvents All() { return WaitEvents(ktl::monostate{}); }
  /// Wait on the set of FdEvents.
  static WaitEvents Fd(FdEvents events) { return WaitEvents(events); }
  /// Wait for the specified value.
  static WaitEvents Value(uint64_t value) { return WaitEvents(value); }

  /// impl WaitEvents

  ///  Returns whether a wait on `self` should be woken up by `other`.
  bool Intercept(const WaitEvents& other) const {
    return ktl::visit(WaitEvents::overloaded{
                          [](const ktl::monostate&, const Variant&) { return true; },
                          [](const Variant&, const ktl::monostate&) { return true; },
                          [](const FdEvents& self, const FdEvents& other) {
                            return (self.bits() & other.bits()) != 0;
                          },
                          [](const uint64_t& self, const uint64_t& other) { return self == other; },
                          [](const auto&, const auto&) { return false; }},
                      data_, other.data_);
  }

  // C++
  const Variant& data() const { return data_; }

  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

 private:
  explicit WaitEvents(Variant data) : data_(ktl::move(data)) {}

  Variant data_;
};

class CurrentTask;
class HandleWaitCanceler;
struct WaitQueueImpl;

/// Implementation of Waiter. We put the Waiter data in an Arc so that WaitQueue can tell when the
/// Waiter has been destroyed by keeping a Weak reference. But this is an implementation detail
/// and a Waiter should have a single owner. So the Arc is hidden inside Waiter.
class PortWaiter : public fbl::RefCountedUpgradeable<PortWaiter> {
 private:
  using CallbackMap = std::map<WaitKey, WaitCallback, std::less<>,
                               util::Allocator<std::pair<const WaitKey, WaitCallback>>>;

  using WaitQueueMap =
      std::map<WaitKey, util::WeakPtr<starnix_sync::StarnixMutex<WaitQueueImpl>>, std::less<>,
               util::Allocator<std::pair<
                   const WaitKey, util::WeakPtr<starnix_sync::StarnixMutex<WaitQueueImpl>>>>>;

  fbl::RefPtr<PortEvent> port_;
  mutable CallbackMap callbacks_;  // the key 0 is reserved for 'no handler'
  AtomicCounter<uint64_t> next_key_;
  bool ignore_signals_;

  /// Collection of wait queues this Waiter is waiting on, so that when the Waiter is Dropped it
  /// can remove itself from the queues.
  ///
  /// This lock is nested inside the WaitQueue.waiters lock.
  starnix_sync::StarnixMutex<WaitQueueMap> wait_queues_;

  /// impl PortWaiter
 public:
  /// Internal constructor.
  static fbl::RefPtr<PortWaiter> New(bool ignore_signals);

  /// Waits until the given deadline has passed or the waiter is woken up. See wait_until().
  fit::result<Errno> WaitInternal(zx_instant_mono_t deadline);

  fit::result<Errno> WaitUntil(const CurrentTask& current_task, zx_instant_mono_t deadline);

  WaitKey NextKey() const {
    uint64_t key = next_key_.next();
    // TODO - find a better reaction to wraparound
    ZX_ASSERT_MSG(key != 0, "bad key from u64 wraparound");
    return WaitKey{.id = key};
  }

  WaitKey RegisterCallback(WaitCallback callback) const;

  ktl::optional<WaitCallback> RemoveCallback(const WaitKey& key) const;

  void WakeImmediately(WaitEvents events, EventHandler handler) const;

  /// Establish an asynchronous wait for the signals on the given Zircon handle (not to be
  /// confused with POSIX signals), optionally running a FnOnce.
  ///
  /// Returns a `HandleWaitCanceler` that can be used to cancel the wait.
  fit::result<zx_status_t, HandleWaitCanceler> WakeOnZirconSignals(const Handle& handle,
                                                                   zx_signals_t zx_signals,
                                                                   SignalHandler handler) const;

  void QueueEvents(const WaitKey& key, WaitEvents events) const;

  void Interrupt() const;

  auto& wait_queues() { return wait_queues_; }
  // const auto& wait_queues() const { return wait_queues_; }

  auto& callbacks() { return callbacks_; }
  const auto& callbacks() const { return callbacks_; }

 private:
  explicit PortWaiter(fbl::RefPtr<PortEvent> port, bool ignore_signals);
};

class AbortHandle : public fbl::RefCountedUpgradeable<AbortHandle> {};

class WaiterKind {
 public:
  using Variant = ktl::variant<util::WeakPtr<PortWaiter>, util::WeakPtr<InterruptibleEvent>,
                               util::WeakPtr<AbortHandle>>;

  static WaiterKind PortWaiter(util::WeakPtr<PortWaiter> waiter) {
    return WaiterKind(ktl::move(waiter));
  }
  static WaiterKind Event(util::WeakPtr<InterruptibleEvent> waiter) {
    return WaiterKind(ktl::move(waiter));
  }

  static WaiterKind AbortHandle(util::WeakPtr<AbortHandle> waiter) {
    return WaiterKind(ktl::move(waiter));
  }

  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

 private:
  friend class WaiterRef;
  explicit WaiterKind(Variant waiter) : waiter_(ktl::move(waiter)) {}

  Variant waiter_;
};

class Waiter;

/// A weak reference to a Waiter. Intended for holding in wait queues or stashing elsewhere for
/// calling queue_events later.
class WaiterRef {
 public:
  static WaiterRef FromPort(fbl::RefPtr<PortWaiter> waiter) {
    return WaiterRef(WaiterKind::PortWaiter(util::WeakPtr(waiter.get())));
  }
  static WaiterRef FromEvent(fbl::RefPtr<InterruptibleEvent> waiter);
  static WaiterRef FromAbortHandle(fbl::RefPtr<AbortHandle> waiter);

  bool IsValid() const {
    return ktl::visit(
        WaiterKind::overloaded{
            [](const util::WeakPtr<PortWaiter>& waiter) { return waiter.Lock() != nullptr; },
            [](const util::WeakPtr<InterruptibleEvent>& event) { return event.Lock() != nullptr; },
            [](const util::WeakPtr<AbortHandle>& handle) { return handle.Lock() != nullptr; }},
        waiter_kind_.waiter_);
  }

  void Interrupt();

  void RemoveCallback(WaitKey key);

  /// Called by the WaitQueue when this waiter is about to be removed from the queue.
  ///
  /// TODO(abarth): This function does not appear to be called when the WaitQueue is dropped,
  /// which appears to be a leak.
  void WillRemoveFromWaitQueue(WaitKey key);

  /// Notify the waiter that the `events` have occurred.
  ///
  /// If the client is using an `SimpleWaiter`, they will be notified but they will not learn
  /// which events occurred.
  ///
  /// If the client is using an `AbortHandle`, `AbortHandle::abort()` will be called.
  bool Notify(WaitKey key, WaitEvents events);

  bool operator==(const Waiter& other) const;
  bool operator==(const fbl::RefPtr<InterruptibleEvent>& other) const;

  bool operator==(const WaiterRef& other) const {
    return ktl::visit(WaiterKind::overloaded{
                          [](const util::WeakPtr<PortWaiter>& lhs,
                             const util::WeakPtr<PortWaiter>& rhs) { return lhs == rhs; },
                          [](const util::WeakPtr<InterruptibleEvent>& lhs,
                             const util::WeakPtr<InterruptibleEvent>& rhs) { return lhs == rhs; },
                          [](const util::WeakPtr<AbortHandle>& lhs,
                             const util::WeakPtr<AbortHandle>& rhs) { return lhs == rhs; },
                          [](const auto&, const auto&) { return false; }},
                      waiter_kind_.waiter_, other.waiter_kind_.waiter_);
  }

 private:
  explicit WaiterRef(WaiterKind waiter) : waiter_kind_(ktl::move(waiter)) {}
  WaiterKind waiter_kind_;
};

/// An entry in a WaitQueue.
struct WaitEntry {
  /// The waiter that is waking for the FdEvent.
  WaiterRef waiter;

  /// The events that the waiter is waiting for.
  WaitEvents filter;

  /// key for cancelling and queueing events
  WaitKey key;
};

struct WaitEntryWithId {
  WaitEntry entry;
  /// The ID use to uniquely identify this wait entry even if it shares the
  /// key used in the wait queue's [`DenseMap`] with another wait entry since
  /// a dense map's keys are recycled.
  uint64_t id;
};

using WaiterMap = util::DenseMap<size_t, WaitEntryWithId>;

struct WaitEntryId {
  WaiterMap::KeyType key;
  uint64_t id;
};

struct WaitQueueImpl {
  /// Holds the next ID value to use when adding a new `WaitEntry` to the
  /// waiters (dense) map.
  ///
  /// A [`DenseMap`]s keys are recycled so we use the ID to uniquely identify
  /// a wait entry.
  uint64_t next_wait_entry_id;

  /// The list of waiters.
  ///
  /// The waiter's wait_queues lock is nested inside this lock.
  WaiterMap waiters;
};

class PortWaiter;
class HandleWaitCanceler {
 public:
  void Cancel(Handle* handle);

 private:
  util::WeakPtr<PortWaiter> waiter_;

  WaitKey key_;
};

class Waiter {
 public:
  /// Create a new waiter.
  static Waiter New();

  /// Create a new waiter that doesn't wake up when a signal is received.
  static Waiter NewIgnoringSignals();

  /// Create a weak reference to this waiter.
  WaiterRef weak() const;

  /// Wait until the waiter is woken up.
  ///
  /// If the wait is interrupted (see [`Waiter::interrupt`]), this function returns EINTR.
  fit::result<Errno> Wait(const CurrentTask& current_task) const;

  /// Wait until the given deadline has passed or the waiter is woken up.
  ///
  /// If the wait deadline is nonzero and is interrupted (see [`Waiter::interrupt`]), this
  /// function returns EINTR. Callers must take special care not to lose any accumulated data or
  /// local state when EINTR is received as this is a normal and recoverable situation.
  ///
  /// Using a 0 deadline (no waiting, useful for draining pending events) will not wait and is
  /// guaranteed not to issue EINTR.
  ///
  /// It the timeout elapses with no events, this function returns ETIMEDOUT.
  ///
  /// Processes at most one event. If the caller is interested in draining the events, it should
  /// repeatedly call this function with a 0 deadline until it reports ETIMEDOUT. (This case is
  /// why a 0 deadline must not return EINTR, as previous calls to wait_until() may have
  /// accumulated state that would be lost when returning EINTR to userspace.)
  ///
  /// It is up to the caller (the "waiter") to make sure that it synchronizes with any object
  /// that triggers an event (the "notifier"). This `Waiter` does not provide any synchronization
  /// itself. Note that synchronization between the "waiter" the "notifier" may be provided by
  /// the [`EventHandler`] used to handle an event iff the waiter observes the side-effects of
  /// the handler (e.g. reading the ready list modified by [`EventHandler::Enqueue`] or
  /// [`EventHandler::EnqueueOnce`]).
  fit::result<Errno> WaitUntil(const CurrentTask& current_task, zx_instant_mono_t deadline) const;

  WaitEntry CreateWaitEntry(WaitEvents filter) const;

  WaiterRef CreateWaitEntryWithHandler(WaitEvents filter, EventHandler handler) const;

  void WakeImmediately(WaitEvents events, EventHandler handler) const;

  /// Establish an asynchronous wait for the signals on the given Zircon handle (not to be
  /// confused with POSIX signals), optionally running a FnOnce.
  ///
  /// Returns a `HandleWaitCanceler` that can be used to cancel the wait.
  fit::result<zx_status_t, HandleWaitCanceler> WakeOnZirconSignals(const Handle& handle,
                                                                   zx_signals_t zx_signals,
                                                                   SignalHandler handler) const;

  /// Return a WaitCanceler representing a wait that will never complete. Useful for stub
  /// implementations that should block forever even though a real implementation would wake up
  /// eventually.
  WaiterRef FakeWait() const;

  /// Interrupt the waiter to deliver a signal. The wait operation will return EINTR, and a
  /// typical caller should then unwind to the syscall dispatch loop to let the signal be
  /// processed. See wait_until() for more details.
  ///
  /// Ignored if the waiter was created with new_ignoring_signals().
  void Interrupt() const;

  // C++
  ~Waiter();

 private:
  friend class WaitQueue;

  explicit Waiter(fbl::RefPtr<PortWaiter> waiter);

  fbl::RefPtr<PortWaiter> inner_;
};

class WaitCanceler;

/// A list of waiters waiting for some event.
///
/// For events that are generated inside Starnix, we walk the wait queue
/// on the thread that triggered the event to notify the waiters that the event
/// has occurred. The waiters will then wake up on their own thread to handle
/// the event.
class WaitQueue {
 public:
  WaitQueue();

  WaitEntryId AddWaiter(WaitEntry entry) const;

  /// Establish a wait for the given entry.
  ///
  /// The waiter will be notified when an event matching the entry occurs.
  ///
  /// This function does not actually block the waiter. To block the waiter,
  /// call the [`Waiter::wait`] function on the waiter.
  ///
  /// Returns a `WaitCanceler` that can be used to cancel the wait.
  WaitCanceler WaitAsyncEntry(const Waiter& waiter, WaitEntry entry) const;

  /// Establish a wait for the given value event.
  ///
  /// The waiter will be notified when an event with the same value occurs.
  ///
  /// This function does not actually block the waiter. To block the waiter,
  /// call the [`Waiter::wait`] function on the waiter.
  ///
  /// Returns a `WaitCanceler` that can be used to cancel the wait.
  WaitCanceler WaitAsyncValue(const Waiter& waiter, uint64_t value) const;

  /// Establish a wait for the given FdEvents.
  ///
  /// The waiter will be notified when an event matching the `events` occurs.
  ///
  /// This function does not actually block the waiter. To block the waiter,
  /// call the [`Waiter::wait`] function on the waiter.
  ///
  /// Returns a `WaitCanceler` that can be used to cancel the wait.
  WaitCanceler WaitAsyncFdEvents(const Waiter& waiter, FdEvents events, EventHandler handler) const;

  /// Establish a wait for any event.
  ///
  /// The waiter will be notified when any event occurs.
  ///
  /// This function does not actually block the waiter. To block the waiter,
  /// call the [`Waiter::wait`] function on the waiter.
  ///
  /// Returns a `WaitCanceler` that can be used to cancel the wait.
  WaitCanceler WaitAsync(const Waiter& waiter) const;

  void WaitAsyncSimple(Waiter& waiter) const;

  size_t NotifyEventsCount(WaitEvents events, size_t limit) const;

  void NotifyFdEvents(FdEvents events) const;

  void NotifyValue(uint64_t value) const {
    NotifyEventsCount(WaitEvents::All(), std::numeric_limits<size_t>::max());
  }

  void NotifyUnorderedCount(size_t limit) const { NotifyEventsCount(WaitEvents::All(), limit); }

  void NotifyAll() const { NotifyUnorderedCount(std::numeric_limits<size_t>::max()); }

  bool IsEmpty() const { return inner_->Lock()->waiters.empty(); }

 private:
  fbl::RefPtr<starnix_sync::StarnixMutex<WaitQueueImpl>> inner_;
};

struct WaitCancelerQueue {
  util::WeakPtr<starnix_sync::StarnixMutex<WaitQueueImpl>> wait_queue;
  WaiterRef waiter;
  WaitKey wait_key;
  WaitEntryId waiter_id;
};

struct WaitCancelerZxio {
  // util::WeakPtr<Zxio> zxio;
  HandleWaitCanceler inner;
};

struct WaitCancelerEvent {
  // util::WeakPtr<zx::Event> event;
  HandleWaitCanceler inner;
};

struct WaitCancelerEventPair {
  // fbl::WeakPtr<zx::EventPair> event_pair;
  HandleWaitCanceler inner;
};

struct WaitCancelerTimer {
  // fbl::WeakPtr<zx::Timer> timer;
  HandleWaitCanceler inner;
};

struct WaitCancelerVmo {
  // fbl::WeakPtr<zx::Vmo> vmo;
  HandleWaitCanceler inner;
};

class WaitCancelerInner {
 public:
  using Variant =
      ktl::variant<ktl::monostate, WaitCancelerQueue, WaitCancelerZxio, WaitCancelerEvent,
                   WaitCancelerEventPair, WaitCancelerTimer, WaitCancelerVmo>;

  static WaitCancelerInner Zxio(HandleWaitCanceler inner);
  static WaitCancelerInner Queue(
      util::WeakPtr<starnix_sync::StarnixMutex<WaitQueueImpl>> wait_queue, WaiterRef waiter,
      WaitKey wait_key, WaitEntryId waiter_id) {
    return WaitCancelerInner(WaitCancelerQueue{
        .wait_queue = wait_queue,
        .waiter = waiter,
        .wait_key = wait_key,
        .waiter_id = waiter_id,
    });
  }
  static WaitCancelerInner Event(HandleWaitCanceler inner);
  static WaitCancelerInner EventPair(HandleWaitCanceler inner);
  static WaitCancelerInner Timer(HandleWaitCanceler inner);
  static WaitCancelerInner Vmo(HandleWaitCanceler inner);

  WaitCancelerInner() = default;

 private:
  explicit WaitCancelerInner(Variant inner) : inner_(ktl::move(inner)) {}

  Variant inner_;
};

constexpr size_t WAIT_CANCELER_COMMON_SIZE = 2;

/// Return values for wait_async methods.
///
/// Calling `cancel` will cancel any running wait.
///
/// Does not implement `Clone` or `Copy` so that only a single canceler exists
/// per wait.
class WaitCanceler {
 public:
  static WaitCanceler NewInner(WaitCancelerInner inner) {
    util::SmallVector<WaitCancelerInner, WAIT_CANCELER_COMMON_SIZE> cancellers;
    cancellers.push_back(ktl::move(inner));
    return WaitCanceler(ktl::move(cancellers));
  }

  static WaitCanceler NewNoop(); /*{
    fbl::Vector<WaitCancelerInner> empty;
    return WaitCanceler(
        ktl::move(util::SmallVector<WaitCancelerInner, WAIT_CANCELER_COMMON_SIZE>::from_vec(
            ktl::move(empty))));
  }*/

  static WaitCanceler NewZxio(HandleWaitCanceler inner);

  static WaitCanceler NewQueue(util::WeakPtr<WaitQueue> wait_queue, WaiterRef waiter,
                               WaitKey wait_key, WaitEntryId waiter_id);

  static WaitCanceler NewEvent(HandleWaitCanceler inner);

  static WaitCanceler NewEventPair(HandleWaitCanceler inner);

  static WaitCanceler NewTimer(HandleWaitCanceler inner);

  static WaitCanceler NewVmo(HandleWaitCanceler inner);

  /// Equivalent to `merge_unbounded`, except that it enforces that the resulting vector of
  /// cancellers is small enough to avoid being separately allocated on the heap.
  ///
  /// If possible, use this function instead of `merge_unbounded`, because it gives us better
  /// tools to keep this code path optimized.
  WaitCanceler Merge(WaitCanceler other);

  /// Creates a new `WaitCanceler` that is equivalent to canceling both its arguments.
  WaitCanceler MergeUnbounded(WaitCanceler other);

  /// Cancel the pending wait.
  ///
  /// Takes `self` by value since a wait can only be canceled once.
  void cancel();

 private:
  explicit WaitCanceler(util::SmallVector<WaitCancelerInner, WAIT_CANCELER_COMMON_SIZE> cancellers)
      : cancellers_(ktl::move(cancellers)) {}

  util::SmallVector<WaitCancelerInner, WAIT_CANCELER_COMMON_SIZE> cancellers_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_WAITER_H_
