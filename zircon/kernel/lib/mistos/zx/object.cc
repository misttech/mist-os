// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/object.h"

#include <lib/mistos/zx/process.h>
#include <zircon/types.h>

#include <object/dispatcher.h>
#include <object/handle.h>
#include <object/wait_signal_observer.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

template <typename T>
zx_status_t object<T>::wait_one(zx_signals_t signals, zx::time deadline,
                                zx_signals_t* pending) const {
  static_assert(object_traits<T>::supports_wait, "Object is not waitable.");

  if (!this->get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  Event event;
  zx_status_t result;
  WaitSignalObserver wait_signal_observer;
  HandleOwner handle = Handle::Make(this->get(), ZX_RIGHT_NONE);

  result = wait_signal_observer.Begin(&event, handle.get(), signals);
  if (result != ZX_OK)
    return result;

  const Deadline slackDeadline(deadline.get(), TimerSlack::none());

  // Event::Wait() will return ZX_OK if already signaled,
  // even if the deadline has passed.  It will return ZX_ERR_TIMED_OUT
  // after the deadline passes if the event has not been
  // signaled.
  result = event.Wait(slackDeadline);

  // Regardless of wait outcome, we must call End().
  auto signals_state = wait_signal_observer.End();

  if (pending) {
    *pending = signals_state;
  }

  if (signals_state & ZX_SIGNAL_HANDLE_CLOSED)
    return ZX_ERR_CANCELED;

  return result;
}

template zx_status_t object<zx::process>::wait_one(zx_signals_t signals, zx::time deadline,
                                                   zx_signals_t* pending) const;

}  // namespace zx
