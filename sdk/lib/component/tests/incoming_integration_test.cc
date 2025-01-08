// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.service.test/cpp/fidl.h>
#include <fidl/fidl.service.test/cpp/wire_test_base.h>
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

class LocalEchoServer : public fidl::testing::WireTestBase<::fidl_service_test::Echo>,
                        public LocalCppComponent {
 public:
  explicit LocalEchoServer(async_dispatcher_t* dispatcher, bool* called)
      : dispatcher_(dispatcher), called_(called) {}

  void OnStart() override {
    ASSERT_EQ(outgoing()
                  ->AddUnmanagedProtocol<fidl_service_test::Echo>(
                      bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure))
                  .status_value(),
              ZX_OK);
    {
      auto result = outgoing()->AddService<fidl_service_test::EchoService>(
          fidl_service_test::EchoService::InstanceHandler({
              .foo = bindings_.CreateHandler(this, dispatcher_,
                                            std::mem_fn(&LocalEchoServer::OnFidlClosed)),
              .bar = bindings_.CreateHandler(this, dispatcher_,
                                            std::mem_fn(&LocalEchoServer::OnFidlClosed)),
          }));
      ASSERT_EQ(result.status_value(), ZX_OK);
    }
  }

  void OnFidlClosed(fidl::UnbindInfo info) {
    if (info.is_user_initiated() || info.is_peer_closed()) {
      return;
    }
    FX_LOGS(ERROR) << "LocalEchoServer server error: " << info;
  }

  void EchoString(EchoStringRequestView request, EchoStringCompleter::Sync& completer) override {
    *called_ = true;
    completer.Reply(request->value);
  }

  bool WasCalled() const { return *called_; }

  virtual void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
  }

 private:
  async_dispatcher_t* dispatcher_;
  bool* called_;
  fidl::ServerBindingGroup<fidl_service_test::Echo> bindings_;
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
      .capabilities = {Protocol{fidl::DiscoverableProtocolName<fidl_service_test::Echo>}},
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
      .capabilities = {Service{fidl_service_test::EchoService::Name}},
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
