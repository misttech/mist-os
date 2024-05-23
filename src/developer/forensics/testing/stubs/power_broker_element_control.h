// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_ELEMENT_CONTROL_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_ELEMENT_CONTROL_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>

#include <functional>
#include <string>
#include <utility>

namespace forensics::stubs {

class PowerBrokerElementControl
    : public fidl::testing::TestBase<fuchsia_power_broker::ElementControl> {
 public:
  PowerBrokerElementControl(fidl::ServerEnd<fuchsia_power_broker::ElementControl> server_end,
                            async_dispatcher_t* dispatcher, std::function<void()> on_closure)
      : binding_(dispatcher, std::move(server_end), this,
                 std::mem_fn(&PowerBrokerElementControl::OnFidlClosed)),
        on_closure_(std::move(on_closure)) {}

  void OnFidlClosed(const fidl::UnbindInfo error) {
    FX_LOGS(ERROR) << error;
    on_closure_();
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    FX_NOTIMPLEMENTED() << name << " is not implemented";
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementControl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FX_NOTIMPLEMENTED() << "Method ordinal '" << metadata.method_ordinal << "' is not implemented";
  }

 private:
  fidl::ServerBinding<fuchsia_power_broker::ElementControl> binding_;
  std::function<void()> on_closure_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_ELEMENT_CONTROL_H_
