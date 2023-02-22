// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/service/test/cpp/fidl_test_base.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fidl/cpp/string.h>
#include <lib/fit/function.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace {

using namespace component_testing;

class LocalEchoServer : public fidl::service::test::testing::Echo_TestBase,
                        public LocalComponentImpl {
 public:
  explicit LocalEchoServer(async_dispatcher_t* dispatcher, bool* called)
      : dispatcher_(dispatcher), called_(called) {}

  void OnStart() override {
    ASSERT_EQ(outgoing()->AddPublicService(bindings_.GetHandler(this, dispatcher_)), ZX_OK);
    sys::ServiceHandler handler;
    fidl::service::test::EchoService::Handler echo_server(&handler);
    ASSERT_EQ(echo_server.add_foo(bindings_.GetHandler(this, dispatcher_)), ZX_OK);
    ASSERT_EQ(outgoing()->AddService<fidl::service::test::EchoService>(std::move(handler)), ZX_OK);
  }

  void EchoString(::fidl::StringPtr value, EchoStringCallback callback) override {
    callback(std::move(value));
    *called_ = true;
  }

  bool WasCalled() const { return *called_; }

  void NotImplemented_(const std::string& name) override {}

 private:
  async_dispatcher_t* dispatcher_;
  bool* called_;
  fidl::BindingSet<fidl::service::test::Echo> bindings_;
};

class IncomingTest : public gtest::RealLoopFixture {};

TEST_F(IncomingTest, ConnectsToProtocolInNamespace) {
  auto realm_builder = RealmBuilder::Create();

  realm_builder.AddChild("echo_client", "#meta/echo_client.cm",
                         ChildOptions{.startup_mode = StartupMode::EAGER});
  bool called = false;
  realm_builder.AddLocalChild("echo_server", [dispatcher = dispatcher(), called_ptr = &called]() {
    return std::make_unique<LocalEchoServer>(dispatcher, called_ptr);
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Protocol{fidl::service::test::Echo::Name_}},
      .source = ChildRef{"echo_server"},
      .targets = {ChildRef{"echo_client"}},
  });

  auto realm = realm_builder.Build(dispatcher());
  auto cleanup = fit::defer([&]() {
    bool complete = false;
    realm.Teardown([&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });
  });

  RunLoopUntil([&called]() { return called; });
}

TEST_F(IncomingTest, ConnectsToServiceInNamespace) {
  auto realm_builder = RealmBuilder::Create();

  realm_builder.AddChild("echo_client", "#meta/echo_service_client.cm",
                         ChildOptions{.startup_mode = StartupMode::EAGER});
  bool called = false;
  realm_builder.AddLocalChild("echo_server", [dispatcher = dispatcher(), called_ptr = &called]() {
    return std::make_unique<LocalEchoServer>(dispatcher, called_ptr);
  });
  realm_builder.AddRoute(Route{
      .capabilities = {Service{fidl::service::test::EchoService::Name}},
      .source = ChildRef{"echo_server"},
      .targets = {ChildRef{"echo_client"}},
  });

  auto realm = realm_builder.Build(dispatcher());
  auto cleanup = fit::defer([&]() {
    bool complete = false;
    realm.Teardown([&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });
  });

  RunLoopUntil([&called]() { return called; });
}

}  // namespace
