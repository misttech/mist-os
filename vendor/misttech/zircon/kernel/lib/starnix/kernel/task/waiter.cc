// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/waiter.h"

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/util/num.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <trace.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/numeric.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

using starnix_sync::NotifyKind;
using starnix_sync::PortWaitResult;

void EventHandler::handle(FdEvents events) const {
  auto enqueue_event_handler = ktl::visit(
      EventHandler::overloaded{
          [](const ktl::monostate&) -> ktl::optional<EnqueueEventHandler> { return {}; },
          [](const Enqueue& e) -> ktl::optional<EnqueueEventHandler> { return e.handler; },
          [](const EnqueueOnce& e) -> ktl::optional<EnqueueEventHandler> {
            return e.handler->Lock()->value();
          },
      },
      handler_);

  if (!enqueue_event_handler) {
    return;
  }

  auto& [key, queue, sought_events, mappings] = *enqueue_event_handler;

  auto e = events & sought_events;
  if (mappings) {
    auto f = *mappings;
    e = f(e);
  }

  fbl::AllocChecker ac;
  queue->Lock()->push_back({key, e}, &ac);
  ZX_ASSERT(ac.check());
}

void SignalHandler::handle(zx_signals_t signals) const {
  LTRACE_ENTRY_OBJ;
  auto [inner, event_handler] = *this;
  auto events = ktl::visit(SignalHandlerInner::overloaded{
                               [&](const ZxioSignalHandler& h) -> ktl::optional<FdEvents> {
                                 return ktl::nullopt;
                                 // return h.GetEventsFromZxioSignals(signals);
                               },
                               [&](const ZxHandleSignalHandler& h) -> ktl::optional<FdEvents> {
                                 return ktl::nullopt;
                                 // return h.GetEventsFromZxSignals(signals);
                               },
                               [&](const ManyZxHandleSignalHandler& h) -> ktl::optional<FdEvents> {
                                 return ktl::nullopt;
                                 // return h.GetEventsFromZxSignals(signals);
                               },
                           },
                           inner.inner_);

  if (events) {
    event_handler.handle(events.value());
  }
  LTRACE_EXIT_OBJ;
}

bool WaitEvents::intercept(const WaitEvents& other) const {
  LTRACE;
  return ktl::visit(
      WaitEvents::overloaded{[](const ktl::monostate&, const ktl::monostate&) { return true; },
                             [](const ktl::monostate&, const FdEvents&) { return true; },
                             [](const ktl::monostate&, const uint64_t&) { return true; },
                             [](const FdEvents&, const ktl::monostate&) { return true; },
                             [](const uint64_t&, const ktl::monostate&) { return true; },
                             [](const FdEvents& self, const FdEvents& other) {
                               return (self.bits() & other.bits()) != 0;
                             },
                             [](uint64_t self, uint64_t other) { return self == other; },
                             [](const auto&, const auto&) { return false; }},
      data_, other.data_);
}

void WaiterRef::interrupt() const {
  LTRACE_ENTRY_OBJ;
  ktl::visit(WaiterKind::overloaded{[](const util::WeakPtr<PortWaiter>& waiter) {
                                      if (auto strong = waiter.Lock()) {
                                        strong->interrupt();
                                      }
                                    },
                                    [](const util::WeakPtr<InterruptibleEvent>& event) {
                                      if (auto strong = event.Lock()) {
                                        // strong->Interrupt();
                                      }
                                    },
                                    [](const util::WeakPtr<AbortHandle>& handle) {
                                      if (auto strong = handle.Lock()) {
                                        // strong->Abort();
                                      }
                                    }},
             waiter_kind_.waiter_);
  LTRACE_EXIT_OBJ;
}

void WaiterRef::will_remove_from_wait_queue(WaitKey key) { LTRACE; }

bool WaiterRef::notify(WaitKey key, WaitEvents events) {
  LTRACE;
  return ktl::visit(
      WaiterKind::overloaded{[&](const util::WeakPtr<PortWaiter>& waiter) -> bool {
                               if (auto strong = waiter.Lock()) {
                                 strong->queue_events(key, events);
                                 return true;
                               }
                               return false;
                             },
                             [&](const util::WeakPtr<InterruptibleEvent>& event) -> bool {
                               if (auto strong = event.Lock()) {
                                 // strong->Notify();
                                 return true;
                               }
                               return false;
                             },
                             [&](const util::WeakPtr<AbortHandle>& handle) -> bool {
                               if (auto strong = handle.Lock()) {
                                 // strong->Abort();
                                 return true;
                               }
                               return false;
                             }},
      waiter_kind_.waiter_);
}

WaitQueue::WaitQueue() {
  LTRACE_ENTRY_OBJ;
  fbl::AllocChecker ac;
  inner_ = fbl::MakeRefCountedChecked<starnix_sync::StarnixMutex<WaitQueueImpl>>(&ac);
  ZX_ASSERT(ac.check());
}

WaitEntryId WaitQueue::add_waiter(WaitEntry entry) const {
  LTRACE_ENTRY_OBJ;
  auto wait_queue = this->inner_->Lock();
  auto optional_id = mtl::checked_add(wait_queue->next_wait_entry_id, 1ul);
  ZX_ASSERT_MSG(optional_id.has_value(), "all possible wait entry ID values exhausted");
  auto id = optional_id.value();
  wait_queue->next_wait_entry_id = id;
  ZX_ASSERT_MSG(wait_queue->waiters.emplace(id, WaitEntryWithId{.entry = entry, .id = id}).second,
                "wait entry ID collision");
  return {.key = id, .id = id};
}

WaitCanceler WaitQueue::wait_async_entry(const Waiter& waiter, WaitEntry entry) const {
  LTRACE_ENTRY_OBJ;
  // profile_duration!("WaitAsyncEntry");
  auto wait_key = entry.key;
  auto waiter_id = this->add_waiter(entry);
  auto wait_queue = util::WeakPtr(this->inner_.get());
  auto [_, inserted] = waiter.inner_->wait_queues_.Lock()->emplace(wait_key, wait_queue);
  ZX_ASSERT_MSG(inserted, "wait key collision");
  return WaitCanceler::new_inner(
      WaitCancelerInner::Queue(WaitCancelerQueue{.wait_queue = wait_queue,
                                                 .waiter = waiter.weak(),
                                                 .wait_key = wait_key,
                                                 .waiter_id = waiter_id}));
}

WaitCanceler WaitQueue::wait_async(const Waiter& waiter) const {
  LTRACE_ENTRY_OBJ;
  return wait_async_entry(waiter, waiter.create_wait_entry(WaitEvents::All()));
}

size_t WaitQueue::notify_events_count(WaitEvents events, size_t limit) const {
  LTRACE_ENTRY_OBJ;
  // profile_duration!("NotifyEventsCount");
  size_t woken = 0;
  auto wait_queue = this->inner_->Lock();
  wait_queue->waiters.key_ordered_retain([&](WaitEntryWithId& entry_with_id) {
    LTRACEF("entry id %lu\n", entry_with_id.id);
    auto& [entry, _] = entry_with_id;
    if (limit > 0 && entry.filter.intercept(events)) {
      if (entry.waiter.notify(entry_with_id.entry.key, events)) {
        limit--;
        woken++;
      }

      entry.waiter.will_remove_from_wait_queue(entry_with_id.entry.key);
      return false;
    }
    return true;
  });

  LTRACEF("woken %zu\n", woken);
  return woken;
}

void WaitQueue::notify_value(uint64_t value) const {
  notify_events_count(WaitEvents::All(), ktl::numeric_limits<size_t>::max());
}

void WaitQueue::notify_unordered_count(size_t limit) const {
  notify_events_count(WaitEvents::All(), limit);
}

void WaitQueue::notify_all() const { notify_unordered_count(ktl::numeric_limits<size_t>::max()); }

bool WaitQueue::is_empty() const { return inner_->Lock()->waiters.empty(); }

WaiterKind::~WaiterKind() {  // LTRACE_ENTRY_OBJ;
}

Waiter Waiter::New() { return Waiter(PortWaiter::New(false)); }

Waiter Waiter::new_ignoring_signals() { return Waiter(PortWaiter::New(true)); }

WaiterRef Waiter::weak() const { return WaiterRef::from_port(inner_); }

fbl::RefPtr<PortWaiter> PortWaiter::New(bool ignore_signals) {
  fbl::AllocChecker ac;
  auto port = fbl::MakeRefCountedChecked<PortEvent>(&ac);
  ZX_ASSERT(ac.check());

  auto waiter = fbl::AdoptRef(new (&ac) PortWaiter(port, ignore_signals));
  ZX_ASSERT(ac.check());
  return waiter;
}

fit::result<Errno> PortWaiter::wait_internal(zx_instant_mono_t deadline) {
  LTRACE_ENTRY_OBJ;
  // This method can block arbitrarily long, possibly waiting for another process. The
  // current thread should not own any local ref that might delay the release of a resource
  // while doing so.
  // debug_assert_no_local_temp_ref();

  // profile_duration!("PortWaiterWaitInternal");

  auto result = port_->Wait(deadline);
  return ktl::visit(
      PortWaitResult::overloaded{
          [](const PortWaitResult::Notification& n) -> fit::result<Errno> {
            if (n.kind == NotifyKind::Regular) {
              return fit::ok();
            }
            return fit::error(errno(EINTR));
          },
          [&](const PortWaitResult::Signal& s) -> fit::result<Errno> {
            if (auto callback = this->remove_callback(WaitKey{s.key})) {
              ktl::visit(WaitCallback::overloaded{
                             [&](const SignalHandler& h) { h.handle(s.observed); },
                             [](const EventHandler&) { ZX_PANIC("wrong type of handler called"); }},
                         callback->callback_);
            }
            return fit::ok();
          },
          [](const PortWaitResult::TimedOut&) -> fit::result<Errno> {
            return fit::error(errno(ETIMEDOUT));
          }},
      result.result());
}

fit::result<Errno> PortWaiter::wait_until(const CurrentTask& current_task,
                                          zx_instant_mono_t deadline) {
  LTRACE_ENTRY_OBJ;
  auto is_waiting = zx_nsec_from_duration(deadline) > 0;

  auto callback = [&]() -> fit::result<Errno> {
    // We are susceptible to spurious wakeups because interrupt() posts a message to the port
    // queue. In addition to more subtle races, there could already be valid messages in the
    // port queue that will immediately wake us up, leaving the interrupt message in the queue
    // for subsequent waits (which by then may not have any signals pending) to read.
    //
    // It's impossible to non-racily guarantee that a signal is pending so there might always
    // be an EINTR result here with no signal. But any signal we get when !is_waiting we know is
    // leftover from before: the top of this function only sets ourself as the
    // current_task.signals.run_state when there's a nonzero timeout, and that waiter reference
    // is what is used to signal the interrupt().
    do {
      auto wait_result = this->wait_internal(deadline);
      if (wait_result.is_error()) {
        if (wait_result.error_value() == errno(EINTR) && !is_waiting) {
          continue;  // Spurious wakeup.
        }
      }
      return wait_result;
    } while (true);
  };

  // Trigger delayed releaser before blocking.
  // current_task.trigger_delayed_releaser();

  if (is_waiting) {
    return current_task.run_in_state(
        RunState::Waiter(WaiterRef::from_port(fbl::RefPtr<PortWaiter>(this))), callback);
  }
  return callback();
}

WaitKey PortWaiter::register_callback(WaitCallback callback) const {
  LTRACE_ENTRY_OBJ;
  WaitKey key = next_key();
  ZX_ASSERT_MSG(callbacks_.Lock()->insert({key, ktl::move(callback)}).second,
                "unexpected callback already present for key %lu", key.id);
  return key;
}

ktl::optional<WaitCallback> PortWaiter::remove_callback(const WaitKey& key) const {
  LTRACE_ENTRY_OBJ;
  auto callbacks = callbacks_.Lock();
  auto iter = callbacks->find(key);
  if (iter == callbacks->end()) {
    return ktl::nullopt;
  }
  auto callback = ktl::move(iter->second);
  callbacks->erase(iter);
  return callback;
}

void PortWaiter::interrupt() const {
  LTRACE_ENTRY_OBJ;
  if (ignore_signals_) {
    return;
  }
  port_->Notify(NotifyKind::Interrupt);
}

void PortWaiter::wake_immediately(FdEvents events, EventHandler handler) const {
  LTRACE_ENTRY_OBJ;
  auto callback = WaitCallback::EventHandlerCallback(ktl::move(handler));
  auto key = register_callback(ktl::move(callback));
  queue_events(key, WaitEvents::Fd(events));
}

void PortWaiter::queue_events(const WaitKey& key, WaitEvents events) const {
  LTRACE_ENTRY_OBJ;
  // profile_duration!("PortWaiterHandleEvent");

  // Defer notification
  auto notify_guard = fit::defer([this]() { port_->Notify(NotifyKind::Regular); });

  // Handling user events immediately when they are triggered breaks any
  // ordering expectations on Linux by batching all starnix events with
  // the first starnix event even if other events occur on the Fuchsia
  // platform (and are enqueued to the `zx::Port`) between them. This
  // ordering does not seem to be load-bearing for applications running on
  // starnix so we take the divergence in ordering in favour of improved
  // performance (by minimizing syscalls) when operating on FDs backed by
  // starnix.
  //
  // TODO(https://fxbug.dev/42084319): If we can read a batch of packets
  // from the `zx::Port`, maybe we can keep the ordering?
  auto callback = remove_callback(key);
  if (!callback.has_value()) {
    return;
  }

  ktl::visit(
      WaitCallback::overloaded{
          [&](const EventHandler& handler) {
            auto fd_events = ktl::visit(
                WaitEvents::overloaded{
                    [&](const ktl::monostate&) -> FdEvents { return FdEvents::all(); },
                    [&](const FdEvents& e) -> FdEvents { return e; },
                    [&](const uint64_t&) -> FdEvents { ZX_PANIC("wrong type of handler called"); }},
                events.data_);
            handler.handle(fd_events);
          },
          [&](const SignalHandler&) { ZX_PANIC("wrong type of handler called"); }},
      callback->callback_);
}

PortWaiter::PortWaiter(fbl::RefPtr<PortEvent> port, bool ignore_signals)
    : port_(ktl::move(port)),
      next_key_(AtomicCounter<uint64_t>::New(1)),
      ignore_signals_(ignore_signals) {
  LTRACE_ENTRY_OBJ;
}

PortWaiter::~PortWaiter() { LTRACE_ENTRY_OBJ; }

Waiter::Waiter(fbl::RefPtr<PortWaiter> waiter) : inner_(ktl::move(waiter)) {}

Waiter::~Waiter() {
  LTRACE_ENTRY_OBJ;
  // Delete ourselves from each wait queue we know we're on to prevent Weak references to
  // ourself from sticking around forever.
  auto wait_queues = ktl::move(*inner_->wait_queues_.Lock());
  for (auto& [_, wait_queue] : wait_queues) {
    if (auto upgraded = wait_queue.Lock()) {
      auto waiters = upgraded->Lock();
      waiters->waiters.key_ordered_retain(
          [this](const WaitEntryWithId& entry) { return entry.entry.waiter != this->weak(); });
    }
  }
}

fit::result<Errno> Waiter::wait(const CurrentTask& current_task) const {
  return inner_->wait_until(current_task, ZX_TIME_INFINITE);
}

fit::result<Errno> Waiter::wait_until(const CurrentTask& current_task,
                                      zx_instant_mono_t deadline) const {
  return inner_->wait_until(current_task, deadline);
}

WaitEntry Waiter::create_wait_entry(WaitEvents filter) const {
  return WaitEntry{
      .waiter = this->weak(),
      .filter = filter,
      .key = inner_->next_key(),
  };
}

WaitEntry Waiter::create_wait_entry_with_handler(WaitEvents filter, EventHandler handler) const {
  WaitKey key = inner_->register_callback(WaitCallback::EventHandlerCallback(ktl::move(handler)));
  return WaitEntry{
      .waiter = this->weak(),
      .filter = filter,
      .key = key,
  };
}

void Waiter::wake_immediately(FdEvents events, EventHandler handler) const {
  inner_->wake_immediately(events, handler);
}

fit::result<zx_status_t, HandleWaitCanceler> Waiter::wake_on_zircon_signals(
    const Handle& handle, zx_signals_t zx_signals, SignalHandler handler) const {
  return inner_->wake_on_zircon_signals(handle, zx_signals, handler);
}

WaitCanceler Waiter::fake_wait() const { return WaitCanceler::new_noop(); }

void Waiter::interrupt() const { inner_->interrupt(); }

}  // namespace starnix
