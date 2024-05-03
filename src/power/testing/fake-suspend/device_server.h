// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_TESTING_FAKE_SUSPEND_DEVICE_SERVER_H_
#define SRC_POWER_TESTING_FAKE_SUSPEND_DEVICE_SERVER_H_

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/natural_types.h>

#include <memory>
#include <optional>
#include <vector>

#include "interfaces.h"

namespace fake_suspend {

// Protocol served to client components over devfs.
class DeviceServer : public fidl::Server<fuchsia_hardware_suspend::Suspender>, public Resumable {
 public:
  explicit DeviceServer(
      std::shared_ptr<std::vector<fuchsia_hardware_suspend::SuspendState>> suspend_states);

  void GetSuspendStates(GetSuspendStatesCompleter::Sync& completer) override;
  void Suspend(SuspendRequest& request, SuspendCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_suspend::Suspender> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  zx::result<> Resume(const DeviceResumeRequest& request) override;

  bool IsSuspended() override { return suspend_completer_.has_value(); }
  std::optional<uint64_t> LastStateIndex() override { return last_state_index_; }

  void Serve(async_dispatcher_t* dispatcher,
             fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> server);

  void set_suspend_observer(std::weak_ptr<SuspendObserver> suspend_observer) {
    suspend_observer_ = suspend_observer;
  }

 private:
  fidl::ServerBindingGroup<fuchsia_hardware_suspend::Suspender> bindings_;
  std::shared_ptr<std::vector<fuchsia_hardware_suspend::SuspendState>> suspend_states_;
  std::weak_ptr<SuspendObserver> suspend_observer_;
  std::optional<uint64_t> last_state_index_;
  std::optional<SuspendCompleter::Async> suspend_completer_;
};

}  // namespace fake_suspend

#endif  // SRC_POWER_TESTING_FAKE_SUSPEND_DEVICE_SERVER_H_
