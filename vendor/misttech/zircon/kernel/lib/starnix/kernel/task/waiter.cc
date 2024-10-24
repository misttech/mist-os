// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/waiter.h"

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/util/num.h>
#include <lib/mistos/util/weak_wrapper.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>

#include <ktl/enforce.h>

namespace starnix {

using starnix_sync::NotifyKind;
using starnix_sync::PortWaitResult;

void EventHandler::Handle(FdEvents events) const {
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

void SignalHandler::Handle(zx_signals_t signals) const {
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
    event_handler.Handle(events.value());
  }
}

void WaiterRef::WillRemoveFromWaitQueue(WaitKey key) {}

bool WaiterRef::Notify(WaitKey key, WaitEvents events) {
  return ktl::visit(
      WaiterKind::overloaded{[&](const util::WeakPtr<PortWaiter>& waiter) -> bool {
                               if (auto strong = waiter.Lock()) {
                                 strong->QueueEvents(key, events);
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
  fbl::AllocChecker ac;
  inner_ = fbl::MakeRefCountedChecked<starnix_sync::StarnixMutex<WaitQueueImpl>>(&ac);
  ZX_ASSERT(ac.check());
}

WaitEntryId WaitQueue::AddWaiter(WaitEntry entry) const {
  auto wait_queue = this->inner_->Lock();
  auto optional_id = mtl::checked_add(wait_queue->next_wait_entry_id, 1ul);
  ZX_ASSERT_MSG(optional_id.has_value(), "all possible wait entry ID values exhausted");
  auto id = optional_id.value();
  wait_queue->next_wait_entry_id = id;
  auto [iter, inserted] =
      wait_queue->waiters.emplace(id, WaitEntryWithId{.entry = entry, .id = id});
  ZX_ASSERT_MSG(inserted, "wait entry ID collision");
  return {.key = id, .id = id};
}

WaitCanceler WaitQueue::WaitAsyncEntry(const Waiter& waiter, WaitEntry entry) const {
  // profile_duration!("WaitAsyncEntry");
  auto wait_key = entry.key;
  auto waiter_id = this->AddWaiter(entry);
  auto wait_queue = util::WeakPtr(this->inner_.get());
  auto [iter, inserted] = waiter.inner_->wait_queues().Lock()->emplace(wait_key, wait_queue);
  ZX_ASSERT_MSG(inserted, "wait key collision");
  return WaitCanceler::NewInner(
      WaitCancelerInner::Queue(wait_queue, waiter.weak(), wait_key, waiter_id));
}

WaitCanceler WaitQueue::WaitAsync(const Waiter& waiter) const {
  return WaitAsyncEntry(waiter, waiter.CreateWaitEntry(WaitEvents::All()));
}

size_t WaitQueue::NotifyEventsCount(WaitEvents events, size_t limit) const {
  // profile_duration!("NotifyEventsCount");
  size_t woken = 0;
  auto wait_queue = this->inner_->Lock();
  wait_queue->waiters.key_ordered_retain([&](WaitEntryWithId& entry_with_id) {
    if (limit > 0 && entry_with_id.entry.filter.Intercept(events)) {
      if (entry_with_id.entry.waiter.Notify(entry_with_id.entry.key, events)) {
        limit--;
        woken++;
      }

      entry_with_id.entry.waiter.WillRemoveFromWaitQueue(entry_with_id.entry.key);
      return false;
    } else {
      return true;
    }
  });

  return woken;
}

Waiter Waiter::New() { return Waiter(PortWaiter::New(false)); }

Waiter Waiter::NewIgnoringSignals() { return Waiter(PortWaiter::New(true)); }

WaiterRef Waiter::weak() const { return WaiterRef::FromPort(inner_); }

fbl::RefPtr<PortWaiter> PortWaiter::New(bool ignore_signals) {
  fbl::AllocChecker ac;
  auto port = fbl::MakeRefCountedChecked<PortEvent>(&ac);
  ZX_ASSERT(ac.check());

  auto waiter = fbl::AdoptRef(new (&ac) PortWaiter(port, ignore_signals));
  ZX_ASSERT(ac.check());
  return waiter;
}

fit::result<Errno> PortWaiter::WaitInternal(zx_instant_mono_t deadline) {
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
            if (auto callback = this->RemoveCallback(WaitKey{s.key})) {
              ktl::visit(WaitCallback::overloaded{
                             [&](const SignalHandler& h) { h.Handle(s.observed); },
                             [](const EventHandler&) { ZX_PANIC("wrong type of handler called"); }},
                         callback->callback());
            }
            return fit::ok();
          },
          [](const PortWaitResult::TimedOut&) -> fit::result<Errno> {
            return fit::error(errno(ETIMEDOUT));
          }},
      result.result());
}

fit::result<Errno> PortWaiter::WaitUntil(const CurrentTask& current_task,
                                         zx_instant_mono_t deadline) {
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
      auto wait_result = this->WaitInternal(deadline);
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
        RunState::Waiter(WaiterRef::FromPort(fbl::RefPtr<PortWaiter>(this))), callback);
  }
  return callback();
}

ktl::optional<WaitCallback> PortWaiter::RemoveCallback(const WaitKey& key) const {
  auto iter = callbacks_.find(key);
  if (iter == callbacks_.end()) {
    return ktl::nullopt;
  }
  auto callback = iter->second;
  callbacks_.erase(iter);
  return callback;
}

void PortWaiter::Interrupt() const {
  if (ignore_signals_) {
    return;
  }
  port_->Notify(NotifyKind::Interrupt);
}

void PortWaiter::QueueEvents(const WaitKey& key, WaitEvents events) const {
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
  auto callback = RemoveCallback(key);
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
                events.data());
            handler.Handle(fd_events);
          },
          [&](const SignalHandler&) { ZX_PANIC("wrong type of handler called"); }},
      callback->callback());
}

PortWaiter::PortWaiter(fbl::RefPtr<PortEvent> port, bool ignore_signals)
    : port_(ktl::move(port)),
      next_key_(AtomicCounter<uint64_t>::New(1)),
      ignore_signals_(ignore_signals) {}

Waiter::Waiter(fbl::RefPtr<PortWaiter> waiter) : inner_(ktl::move(waiter)) {}

Waiter::~Waiter() = default;

fit::result<Errno> Waiter::Wait(const CurrentTask& current_task) const {
  return inner_->WaitUntil(current_task, ZX_TIME_INFINITE);
}

fit::result<Errno> Waiter::WaitUntil(const CurrentTask& current_task,
                                     zx_instant_mono_t deadline) const {
  return inner_->WaitUntil(current_task, deadline);
}

WaitEntry Waiter::CreateWaitEntry(WaitEvents filter) const {
  return WaitEntry{
      .waiter = this->weak(),
      .filter = filter,
      .key = inner_->NextKey(),
  };
}

}  // namespace starnix
