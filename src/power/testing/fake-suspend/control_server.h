// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_TESTING_FAKE_SUSPEND_CONTROL_SERVER_H_
#define SRC_POWER_TESTING_FAKE_SUSPEND_CONTROL_SERVER_H_

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/natural_types.h>
#include <lib/async/dispatcher.h>
#include <lib/fit/result.h>
#include <lib/zx/result.h>

#include <memory>
#include <optional>
#include <vector>

#include "interfaces.h"

namespace fake_suspend {

// Protocol served to client components over devfs.
class ControlServer : public fidl::Server<test_suspendcontrol::Device>, public SuspendObserver {
 public:
  explicit ControlServer(
      std::shared_ptr<std::vector<fuchsia_hardware_suspend::SuspendState>> suspend_states);

  void SetSuspendStates(SetSuspendStatesRequest& request,
                        SetSuspendStatesCompleter::Sync& completer) override;
  void AwaitSuspend(AwaitSuspendCompleter::Sync& completer) override;
  void Resume(ResumeRequest& request, ResumeCompleter::Sync& completer) override;

  void OnSuspend(std::optional<uint64_t> state_index) override;

  void Serve(async_dispatcher_t* dispatcher, fidl::ServerEnd<test_suspendcontrol::Device> server);

  void set_resumable(std::weak_ptr<Resumable> resumable) { resumable_ = resumable; }

 private:
  fidl::ServerBindingGroup<test_suspendcontrol::Device> bindings_;
  std::shared_ptr<std::vector<fuchsia_hardware_suspend::SuspendState>> suspend_states_;
  std::weak_ptr<Resumable> resumable_;
  std::optional<AwaitSuspendCompleter::Async> await_suspend_completer_;
};

}  // namespace fake_suspend

#endif  // SRC_POWER_TESTING_FAKE_SUSPEND_CONTROL_SERVER_H_
