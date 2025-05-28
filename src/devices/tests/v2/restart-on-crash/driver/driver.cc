// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.crashdriver.test/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace {

class RestartOnCrashDriver : public fdf::DriverBase,
                             public fidl::Server<fuchsia_crashdriver_test::Crasher> {
 public:
  RestartOnCrashDriver(fdf::DriverStartArgs start_args,
                       fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("restart_on_crash", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    // Create an event to get a random number from its koid.
    zx_status_t status = zx::event::create(0, &event_);
    if (status != ZX_OK) {
      return zx::make_result(status);
    }

    zx_info_handle_basic_t info;
    status = event_.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
    if (status != ZX_OK) {
      return zx::make_result(status);
    }

    // Store its koid to return in pong.
    pong_ = info.koid;

    fuchsia_crashdriver_test::Device::InstanceHandler handler({
        .crasher =
            [this](fidl::ServerEnd<fuchsia_crashdriver_test::Crasher> server_end) {
              bindings_.AddBinding(dispatcher(), std::move(server_end), this,
                                   fidl::kIgnoreBindingClosure);
            },
    });

    return outgoing()->AddService<fuchsia_crashdriver_test::Device>(std::move(handler));
  }

  void Crash(CrashCompleter::Sync& completer) override {
    // Crash the process by triggering bad handle policy.
    zx_object_signal(0x534343, 0, 0);
  }

  void Ping(PingCompleter::Sync& completer) override {
    // Return the pong_ we created during start.
    completer.Reply({pong_});
  }

 private:
  zx::event event_ = {};
  uint64_t pong_ = 0;
  fidl::ServerBindingGroup<fuchsia_crashdriver_test::Crasher> bindings_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(RestartOnCrashDriver);
