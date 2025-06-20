// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fidl/cpp/binding.h>

#include "echo_connection.h"

// [START test_imports]
#include <fidl/fidl.examples.routing.echo/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/testing/cpp/inspect.h>

#include <gtest/gtest.h>

using namespace inspect::testing;
// [END test_imports]

namespace example {

class EchoConnectionTest : public testing::Test {
 public:
  EchoConnectionTest()
      : inspector_(),
        stats_{std::make_shared<EchoConnectionStats>(EchoConnectionStats{
            inspector_.GetRoot().CreateUint("bytes_processed", 0),
            inspector_.GetRoot().CreateUint("total_requests", 0),
        })},
        connection_(stats_),
        serverLoop_(async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread)) {}

  void SetUp() override {
    serverLoop_.StartThread("EchoConnectionServer");

    auto [client_end, server_end] =
        fidl::CreateEndpoints<fidl_examples_routing_echo::Echo>().value();

    fidl::BindServer(serverLoop_.dispatcher(), std::move(server_end), &connection_);

    echoClient_.emplace(fidl::SyncClient(std::move(client_end)));
  }

 protected:
  inspect::Inspector inspector_;
  std::shared_ptr<EchoConnectionStats> stats_;
  EchoConnection connection_;
  async::Loop serverLoop_;
  std::optional<fidl::SyncClient<fidl_examples_routing_echo::Echo>> echoClient_;
};

TEST_F(EchoConnectionTest, EchoServerWritesStats) {
  // Invoke the echo server
  ::fidl::StringPtr message;
  message = echoClient_.value()->EchoString({"Hello World!"}).value().response().value();
  message = echoClient_.value()->EchoString({"Hello World!"}).value().response().value();

  // [START inspect_test]
  // Validate the contents of the tree match
  auto hierarchy_result = inspect::ReadFromVmo(inspector_.DuplicateVmo());
  ASSERT_TRUE(hierarchy_result.is_ok());
  EXPECT_THAT(hierarchy_result.take_value(),
              NodeMatches(AllOf(PropertyList(::testing::UnorderedElementsAre(
                  UintIs("bytes_processed", 24), UintIs("total_requests", 2))))));
  // [END inspect_test]
}

}  // namespace example
