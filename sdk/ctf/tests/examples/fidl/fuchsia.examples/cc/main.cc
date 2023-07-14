// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// [START example]
#include <fuchsia/examples/cpp/fidl.h>
#include <fuchsia/testing/harness/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include <string>

#include <test/example/cpp/fidl.h>
#include <zxtest/zxtest.h>

class FuchsiaExamplesTest : public zxtest::Test {
 public:
  ~FuchsiaExamplesTest() override = default;
};

TEST(FuchsiaExamplesTest, Echo) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  test::example::RealmFactorySyncPtr realm_factory;
  auto context = sys::ComponentContext::Create();
  EXPECT_OK(context->svc()->Connect(realm_factory.NewRequest()));

  fuchsia::testing::harness::RealmProxySyncPtr realm_proxy;
  {
    fuchsia::testing::harness::RealmFactory_CreateRealm_Result result;
    EXPECT_OK(realm_factory->CreateRealm(realm_proxy.NewRequest(), &result));
  }

  fuchsia::examples::EchoSyncPtr echo;
  {
    fuchsia::testing::harness::RealmProxy_ConnectToNamedProtocol_Result result;
    EXPECT_OK(realm_proxy->ConnectToNamedProtocol("fuchsia.examples.Echo",
                                                  echo.NewRequest().TakeChannel(), &result));
  }

  std::string response;
  EXPECT_OK(echo->EchoString("hello", &response));
  EXPECT_STREQ(response.c_str(), "hello");
}
// [END example]
