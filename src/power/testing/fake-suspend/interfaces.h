// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_TESTING_FAKE_SUSPEND_INTERFACES_H_
#define SRC_POWER_TESTING_FAKE_SUSPEND_INTERFACES_H_

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/natural_types.h>
#include <lib/zx/result.h>
#include <zircon/time.h>

#include <memory>
#include <optional>
#include <vector>

namespace fake_suspend {

using test_suspendcontrol::DeviceResumeRequest;

class SuspendObserver {
 public:
  virtual void OnSuspend(std::optional<uint64_t> state_index) = 0;
};

class Resumable {
 public:
  virtual bool IsSuspended() = 0;
  virtual std::optional<uint64_t> LastStateIndex() = 0;
  virtual zx::result<> Resume(const DeviceResumeRequest& request) = 0;
};

}  // namespace fake_suspend

#endif  // SRC_POWER_TESTING_FAKE_SUSPEND_INTERFACES_H_
