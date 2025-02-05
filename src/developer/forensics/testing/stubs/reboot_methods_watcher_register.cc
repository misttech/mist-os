// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/testing/stubs/reboot_methods_watcher_register.h"

namespace forensics {
namespace stubs {

void RebootMethodsWatcherRegister::Register(
    ::fidl::InterfaceHandle<fuchsia::hardware::power::statecontrol::RebootMethodsWatcher> watcher) {
  // TODO(https://fxbug.dev/385742868): Remove this once the method is deleted
  // from the API.
  FX_LOGS(FATAL) << "Register is deprecated and should not be used";
}

void RebootMethodsWatcherRegister::RegisterWatcher(
    ::fidl::InterfaceHandle<fuchsia::hardware::power::statecontrol::RebootWatcher> watcher,
    RegisterWatcherCallback callback) {
  watcher_ = watcher.Bind();
  callback();

  fuchsia::hardware::power::statecontrol::RebootOptions options;
  std::vector<fuchsia::hardware::power::statecontrol::RebootReason2> reasons = {reason_};
  options.set_reasons(reasons);
  watcher_->OnReboot(std::move(options), [] {});
}

void RebootMethodsWatcherRegisterHangs::Register(
    ::fidl::InterfaceHandle<fuchsia::hardware::power::statecontrol::RebootMethodsWatcher> watcher) {
  // TODO(https://fxbug.dev/385742868): Remove this once the method is deleted
  // from the API.
  FX_LOGS(FATAL) << "Register is deprecated and should not be used";
}

void RebootMethodsWatcherRegisterHangs::RegisterWatcher(
    ::fidl::InterfaceHandle<fuchsia::hardware::power::statecontrol::RebootWatcher> watcher,
    RegisterWatcherCallback callback) {
  watcher_ = watcher.Bind();
  callback();
}

}  // namespace stubs
}  // namespace forensics
