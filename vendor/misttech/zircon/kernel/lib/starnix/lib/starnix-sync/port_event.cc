// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/starnix_sync/port_event.h"

#include <trace.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls/policy.h>
#include <zircon/types.h>

#include <object/handle.h>
#include <object/port_dispatcher.h>
#include <object/process_dispatcher.h>

#define LOCAL_TRACE 0

namespace starnix_sync {

PortEvent::PortEvent() : event_() {
  zx_status_t result = PortDispatcher::Create(0, &port_, &rights_);
  ZX_ASSERT(result == ZX_OK);
}

PortWaitResult PortEvent::Wait(zx_instant_mono_t deadline) {
  LTRACE_ENTRY_OBJ;

  // TODO (Herrera) : Revisit slack deadline
  const Deadline slackDeadline(deadline, TimerSlack::none());

  auto state = state_.load(ORDERING_FOR_ATOMICS_BETWEEN_NOTIFIER_AND_NOTIFEE);

  bool run = true;
  do {
    LTRACEF_LEVEL(2, "state %d\n", state);
    switch (state) {
      case FUTEX_WAITING: {
        zx_status_t status = event_.Wait(slackDeadline);
        if (status == ZX_OK || status == ZX_ERR_BAD_STATE) {
          state = state_.load(ORDERING_FOR_ATOMICS_BETWEEN_NOTIFIER_AND_NOTIFEE);
        } else if (status == ZX_ERR_TIMED_OUT) {
          return PortWaitResult::TimedOutResult();
        } else {
          PANIC("Unexpected error from zx_futex_wait: %d", status);
        }
        break;
      }
      case FUTEX_NOTIFIED:
      case FUTEX_INTERRUPTED: {
        uint8_t expected_state = state;
        uint8_t new_state = FUTEX_WAITING;
        if (state_.compare_exchange_strong(expected_state, new_state,
                                           ORDERING_FOR_ATOMICS_BETWEEN_NOTIFIER_AND_NOTIFEE,
                                           ORDERING_FOR_ATOMICS_BETWEEN_NOTIFIER_AND_NOTIFEE)) {
          ZX_DEBUG_ASSERT(expected_state == state);
          return (expected_state == FUTEX_INTERRUPTED) ? PortWaitResult::NotifyInterrupt()
                                                       : PortWaitResult::NotifyRegular();
        } else {
          ZX_DEBUG_ASSERT(expected_state != state);
          state = expected_state;
        }
        break;
      }
      case FUTEX_USE_PORT:
        run = false;
        break;
      default:
        __UNREACHABLE;
    }
  } while (run);

  zx_port_packet_t pp;
  zx_status_t st = port_.dispatcher()->Dequeue(slackDeadline, &pp);
  if (st == ZX_OK) {
    switch (pp.status) {
      case ZX_OK: {
        switch (pp.type) {
          case ZX_PKT_TYPE_SIGNAL_ONE:
            return PortWaitResult::SignalResult(pp.key, pp.signal.observed);
          case ZX_PKT_TYPE_USER:
            // User packet w/ OK status is only used to wake up
            // the waiter after handling process-internal events.
            //
            // Note that we can be woken up even when we will
            // not handle any user events. This is because right
            // after we set `has_pending_user_packet` to `false`,
            // another thread can immediately queue a new user
            // event and set `has_pending_user_packet` to `true`.
            // However, that event will be handled by us (by the
            // caller when this method returns) as if the event
            // was enqueued before we received this user packet.
            // Once the caller handles all the current user events,
            // we end up with no remaining user events but a user
            // packet sitting in the `zx::Port`.
            ZX_ASSERT(has_pending_user_packet_.exchange(false));
            return PortWaitResult::NotifyRegular();
          default:
            PANIC("unexpected packet type: %d ", pp.type);
        }
      }
      case ZX_ERR_CANCELED:
        return PortWaitResult::NotifyInterrupt();
      default:
        PANIC("Unexpected status in port wait: %d", st);
    }
  } else {
    if (st == ZX_ERR_TIMED_OUT) {
      return PortWaitResult::TimedOutResult();
    }
    PANIC("Unexpected error from port_wait: %d", st);
  }
}

fit::result<zx_status_t> PortEvent::ObjectWaitAsync(fbl::RefPtr<Dispatcher> handle, uint64_t key,
                                                    zx_signals_t signals, uint32_t opts) {
  uint8_t state = state_.exchange(FUTEX_USE_PORT, ktl::memory_order_acq_rel);
  switch (state) {
    case FUTEX_WAITING:
      event_.Signal();
      break;
    case FUTEX_NOTIFIED:
    case FUTEX_INTERRUPTED:
      QueueUserPacketData(state == FUTEX_INTERRUPTED ? NotifyKind::Interrupt : NotifyKind::Regular);
      break;
    case FUTEX_USE_PORT:
      break;
    default:
      PANIC("unexpected value: %d", state);
  }

  zx_rights_t rights = {};
  auto h = Handle::Make(handle, rights);
  zx_status_t status = port_.dispatcher()->MakeObserver(opts, h.get(), key, signals);
  if (status != ZX_OK) {
    return fit::error(status);
  }
  return fit::ok();
}

void PortEvent::Cancel(fbl::RefPtr<Dispatcher> dispatcher, uint64_t key) {
  zx_rights_t rights = {};
  auto handle = Handle::Make(dispatcher, rights);
}

void PortEvent::QueueUserPacketData(NotifyKind kind) {
  zx_status_t status;
  switch (kind) {
    case NotifyKind::Interrupt:
      status = ZX_ERR_CANCELED;
      break;
    case NotifyKind::Regular:
      if (has_pending_user_packet_.exchange(true, ktl::memory_order_acq_rel)) {
        return;
      }
      status = ZX_OK;
      break;
  }

  zx_port_packet_t packet = {};
  packet.key = 0;
  packet.type = ZX_PKT_TYPE_USER;
  packet.status = status;
  zx_status_t result = port_.dispatcher()->QueueUser(packet);
  ZX_ASSERT(result == ZX_OK);
}

void PortEvent::Notify(NotifyKind kind) {
  uint8_t futex_val = (kind == NotifyKind::Interrupt) ? FUTEX_INTERRUPTED : FUTEX_NOTIFIED;

  LTRACEF_LEVEL(2, "futex_val %d\n", futex_val);
  uint8_t expected = FUTEX_WAITING;
  if (state_.compare_exchange_strong(expected, futex_val, ktl::memory_order_acq_rel,
                                     ktl::memory_order_acquire)) {
    event_.Signal();
  } else {
    switch (expected) {
      case FUTEX_WAITING:
        __UNREACHABLE;
      case FUTEX_NOTIFIED:
      case FUTEX_INTERRUPTED:
        break;
      case FUTEX_USE_PORT:
        QueueUserPacketData(kind);
        break;
      default:
        PANIC("unexpected value: %d", expected);
    }
  }
}

}  // namespace starnix_sync
