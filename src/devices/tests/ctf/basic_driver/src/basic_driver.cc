// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.basicdriver.ctftest/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace {

class BasicDriver : public fdf::DriverBase,
                    public fidl::WireServer<fuchsia_basicdriver_ctftest::Device> {
 public:
  BasicDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("basic", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    fuchsia_basicdriver_ctftest::Service::InstanceHandler handler({
        .device = bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure),
    });
    ZX_ASSERT(
        outgoing()->AddService<fuchsia_basicdriver_ctftest::Service>(std::move(handler)).is_ok());

    SendAck();
    FDF_LOG(INFO, "Started driver!");
    return zx::ok();
  }

  void SendAck() {
    FDF_LOG(INFO, "Sending ack!");
    auto waiter = incoming()->Connect<fuchsia_basicdriver_ctftest::Waiter>();
    ZX_ASSERT(waiter.is_ok());
    ZX_ASSERT(fidl::Call(waiter.value())->Ack().is_ok());
  }

  void Ping(PingCompleter::Sync& completer) override {
    FDF_LOG(INFO, "Replying to Ping");
    completer.Reply(42u);
  }

 private:
  fidl::ServerBindingGroup<fuchsia_basicdriver_ctftest::Device> bindings_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(BasicDriver);
