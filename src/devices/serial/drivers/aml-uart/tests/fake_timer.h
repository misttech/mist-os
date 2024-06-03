// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_AML_UART_TESTS_FAKE_TIMER_H_
#define SRC_DEVICES_SERIAL_DRIVERS_AML_UART_TESTS_FAKE_TIMER_H_

#include <lib/zx/event.h>
#include <lib/zx/timer.h>
#include <zircon/types.h>

// A fake timer class that gives the uniitest more control on the timer state, so that the timer
// behavior can be synchronized better with the test flow, also the tests can get more insights on
// the timer states.
class FakeTimer {
 public:
  explicit FakeTimer();

  void CreateEvent();
  void Clear();
  void FireTimer();

  ~FakeTimer();

  static zx_handle_t timer_handle_;
  static zx_time_t current_deadline_;
  static bool cancel_called_;
};

#endif  // SRC_DEVICES_SERIAL_DRIVERS_AML_UART_TESTS_FAKE_TIMER_H_
