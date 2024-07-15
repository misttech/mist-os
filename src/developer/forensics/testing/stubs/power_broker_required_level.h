// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_REQUIRED_LEVEL_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_REQUIRED_LEVEL_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/async/dispatcher.h>

#include <optional>
#include <utility>

#include "src/developer/forensics/testing/stubs/fidl_server.h"

namespace forensics::stubs {

class PowerBrokerRequiredLevel : public FidlServer<fuchsia_power_broker::RequiredLevel> {
 public:
  PowerBrokerRequiredLevel(fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> server_end,
                           async_dispatcher_t* dispatcher, uint8_t initial_level)
      : binding_(dispatcher, std::move(server_end), this, &PowerBrokerRequiredLevel::OnFidlClosed),
        level_(initial_level),
        level_changed_(true) {}

  void Watch(WatchCompleter::Sync& completer) override;

  void SetLevel(uint8_t level);

 private:
  static void OnFidlClosed(const fidl::UnbindInfo error) { FX_LOGS(ERROR) << error; }

  fidl::ServerBinding<fuchsia_power_broker::RequiredLevel> binding_;
  uint8_t level_;
  bool level_changed_;
  std::optional<WatchCompleter::Async> queued_response_;
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_POWER_BROKER_REQUIRED_LEVEL_H_
