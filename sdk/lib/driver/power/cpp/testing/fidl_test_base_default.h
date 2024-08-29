// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_TESTING_FIDL_TEST_BASE_DEFAULT_H_
#define LIB_DRIVER_POWER_CPP_TESTING_FIDL_TEST_BASE_DEFAULT_H_

#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>

#include <gtest/gtest.h>

namespace fdf_power::testing {

template <typename Protocol>
class FidlTestBaseDefault : public fidl::testing::TestBase<Protocol> {
 public:
  FidlTestBaseDefault(async_dispatcher_t* dispatcher, fidl::ServerEnd<Protocol> server_end)
      : binding_(fidl::BindServer(
            dispatcher, std::move(server_end), this,
            [](FidlTestBaseDefault*, fidl::UnbindInfo, fidl::ServerEnd<Protocol>) {})) {}

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) final {
    FAIL() << "Unexpected call: " << name;
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<Protocol> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) final {
    FAIL() << "Encountered unknown method";
  }

 private:
  fidl::ServerBindingRef<Protocol> binding_;
};

}  // namespace fdf_power::testing

#endif  // LIB_DRIVER_POWER_CPP_TESTING_FIDL_TEST_BASE_DEFAULT_H_
