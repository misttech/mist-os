// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_TESTING_FAKE_CURRENT_LEVEL_H_
#define LIB_DRIVER_POWER_CPP_TESTING_FAKE_CURRENT_LEVEL_H_

#include <fidl/fuchsia.power.broker/cpp/test_base.h>
#include <lib/fidl/cpp/wire/channel.h>

#include "sdk/lib/driver/power/cpp/testing/fidl_test_base_default.h"

namespace fdf_power::testing {

using fuchsia_power_broker::CurrentLevel;
using fuchsia_power_broker::PowerLevel;

class FakeCurrentLevel : public FidlTestBaseDefault<CurrentLevel> {
 public:
  FakeCurrentLevel(async_dispatcher_t* dispatcher, fidl::ServerEnd<CurrentLevel> server_end,
                   PowerLevel initial_level)
      : FidlTestBaseDefault(dispatcher, std::move(server_end)), current_level_(initial_level) {}

  // This function can be made const, but then it doesn't work with DispatcherBound::AsyncCall().
  PowerLevel current_level() { return current_level_; }

 private:
  void Update(UpdateRequest& request, UpdateCompleter::Sync& completer) override {
    current_level_ = request.current_level();
    completer.Reply(fit::ok());
  }

  PowerLevel current_level_;
};

}  // namespace fdf_power::testing

#endif  // LIB_DRIVER_POWER_CPP_TESTING_FAKE_CURRENT_LEVEL_H_
