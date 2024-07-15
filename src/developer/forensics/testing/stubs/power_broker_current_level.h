// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_CURRENT_LEVEL_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_CURRENT_LEVEL_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/syslog/cpp/macros.h>

#include <utility>

#include "src/developer/forensics/testing/stubs/fidl_server.h"

namespace forensics::stubs {

class PowerBrokerCurrentLevel : public FidlServer<fuchsia_power_broker::CurrentLevel> {
 public:
  PowerBrokerCurrentLevel(fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> server_end,
                          async_dispatcher_t* dispatcher, uint8_t initial_level)
      : binding_(dispatcher, std::move(server_end), this, &PowerBrokerCurrentLevel::OnFidlClosed),
        level_(initial_level) {}

  void Update(UpdateRequest& request, UpdateCompleter::Sync& completer) override {
    level_ = request.current_level();
    completer.Reply(fit::ok());
  }

  uint8_t GetLevel() const { return level_; }

 private:
  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

  fidl::ServerBinding<fuchsia_power_broker::CurrentLevel> binding_;
  uint8_t level_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_CURRENT_LEVEL_H_
