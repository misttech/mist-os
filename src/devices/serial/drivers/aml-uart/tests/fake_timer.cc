// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/tests/fake_timer.h"

#include <stdio.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

extern "C" {
// Hijack the |zx_timer_set| and |zx_timer_cancel| syscalls if the input handle is from the timer in
// aml-uart driver, otherwise, execute the original syscall.
__EXPORT zx_status_t zx_timer_set(zx_handle_t handle, zx_time_t deadline, zx_duration_t slack) {
  if (handle != FakeTimer::timer_handle_) {
    return _zx_timer_set(handle, deadline, slack);
  }
  FakeTimer::current_deadline_ = deadline;
  return ZX_OK;
}

__EXPORT zx_status_t zx_timer_cancel(zx_handle_t handle) {
  if (handle != FakeTimer::timer_handle_) {
    return _zx_timer_cancel(handle);
  }

  FakeTimer::current_deadline_ = 0;
  FakeTimer::cancel_called_ = true;
  return ZX_OK;
}
}

zx_handle_t FakeTimer::timer_handle_ = ZX_HANDLE_INVALID;
zx_duration_t FakeTimer::current_deadline_ = 0;
bool FakeTimer::cancel_called_ = false;

// When |FakeTimer| is instantiated, create the fake timer event that will be used to inject the
// driver timer by test cases.
FakeTimer::FakeTimer() { CreateEvent(); }

void FakeTimer::CreateEvent() {
  zx::event driver_end;
  if (zx_status_t status = zx::event::create(0, &driver_end); status != ZX_OK) {
    ZX_PANIC("%s", zx_status_get_string(status));
  }
  timer_handle_ = driver_end.release();
}

void FakeTimer::FireTimer() {
  // Sleep the caller thread until the deadline, so that the time check in
  // |AmlUart::HandleLeaseTimer| will pass.
  ZX_ASSERT(current_deadline_ != 0);
  zx_nanosleep(current_deadline_);

  // This signal works is based on the fact that |ZX_EVENT_SIGNALED| and |ZX_TIMER_SIGNALED| have
  // the same value.
  static_assert(ZX_EVENT_SIGNALED == ZX_TIMER_SIGNALED);
  zx_status_t status = zx_object_signal(timer_handle_, 0, ZX_EVENT_SIGNALED);
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to signal timer object: %d", status);
  current_deadline_ = 0;
}

void FakeTimer::Clear() {
  timer_handle_ = ZX_HANDLE_INVALID;
  current_deadline_ = 0;
  cancel_called_ = false;
}

// Reset the static variables when the |FakeTimer| instance gets destroyed.
FakeTimer::~FakeTimer() { Clear(); }
