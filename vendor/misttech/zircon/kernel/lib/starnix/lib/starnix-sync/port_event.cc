// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/starnix_sync/port_event.h"

#include <zircon/assert.h>
#include <zircon/syscalls/policy.h>

#include <object/port_dispatcher.h>
#include <object/process_dispatcher.h>

#include "object/handle.h"
#include "zircon/errors.h"
#include "zircon/rights.h"
#include "zircon/types.h"

namespace starnix_sync {

PortEvent::PortEvent() {
  zx_status_t result = PortDispatcher::Create(0, &port_, &rights_);
  ZX_ASSERT(result == ZX_OK);
}

PortWaitResult PortEvent::Wait(zx_instant_mono_t deadline) {
  // TODO (Herrera) : Revisit slack deadline
  const Deadline slackDeadline(deadline, TimerSlack::none());

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

}  // namespace starnix_sync
