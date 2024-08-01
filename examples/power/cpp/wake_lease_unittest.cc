// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/power/cpp/wake_lease.h"

#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/result.h>
#include <zircon/types.h>

#include <memory>
#include <unordered_map>

#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/test_loop_fixture.h>

namespace {

using fuchsia_power_system::ActivityGovernor;
using fuchsia_power_system::WakeLeaseToken;

class WakeLeaseTest : public gtest::TestLoopFixture {};

class FakeActivityGovernor : public fidl::Server<ActivityGovernor> {
 public:
  FakeActivityGovernor(async_dispatcher_t* dispatcher, fidl::ServerEnd<ActivityGovernor> server_end)
      : binding_(dispatcher, std::move(server_end), this, [](fidl::UnbindInfo) {}),
        dispatcher_(dispatcher) {}

  bool HasActiveWakeLease() const { return !active_wake_leases_.empty(); }

 private:
  void TakeWakeLease(TakeWakeLeaseRequest& /* ignored */,
                     TakeWakeLeaseCompleter::Sync& completer) override {
    WakeLeaseToken client_token, server_token;
    WakeLeaseToken::create(/*options=*/0u, &client_token, &server_token);

    // Start an async task to wait for EVENTPAIR_PEER_CLOSED signal on server_token.
    zx_handle_t token_handle = server_token.get();
    active_wake_leases_[token_handle] = std::move(server_token);
    auto wait = std::make_unique<async::WaitOnce>(token_handle, ZX_EVENTPAIR_PEER_CLOSED);
    wait->Begin(dispatcher_, [this, token_handle, wait = std::move(wait)](
                                 async_dispatcher_t*, async::WaitOnce*, zx_status_t status,
                                 const zx_packet_signal_t*) {
      EXPECT_EQ(status, ZX_OK);
      auto it = active_wake_leases_.find(token_handle);
      EXPECT_NE(it, active_wake_leases_.end());
      EXPECT_EQ(token_handle, it->second.get());
      active_wake_leases_.erase(it);
    });

    completer.Reply(std::move(client_token));
  }

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    FAIL() << "Unexpected call";
  }
  void RegisterListener(RegisterListenerRequest& request,
                        RegisterListenerCompleter::Sync& completer) override {
    FAIL() << "Unexpected call";
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<ActivityGovernor> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    FAIL() << "Unexpected call";
  }

  fidl::ServerBinding<ActivityGovernor> binding_;
  async_dispatcher_t* dispatcher_;

  std::unordered_map<zx_handle_t, WakeLeaseToken> active_wake_leases_;
};

TEST_F(WakeLeaseTest, TakeWakeLeaseThenDropIt) {
  auto endpoints = fidl::CreateEndpoints<ActivityGovernor>().value();
  FakeActivityGovernor server(test_loop().dispatcher(), std::move(endpoints.server));
  ASSERT_FALSE(server.HasActiveWakeLease());

  // Create the ActivityGovernor client and use it to take a WakeLease.
  fidl::Client<ActivityGovernor> client(std::move(endpoints.client), test_loop().dispatcher());
  examples::power::WakeLease::Take(client, "test-wake-lease")
      .then(
          // Verify that the client received its WakeLease, then immediately destroy it.
          [&server](fpromise::result<examples::power::WakeLease, examples::power::Error>& result) {
            EXPECT_FALSE(result.is_error()) << result.error();
            EXPECT_TRUE(result.is_ok());
            ASSERT_TRUE(server.HasActiveWakeLease());
            // Destroy the WakeLease at the end of this scope.
            auto wake_lease = result.take_value();
          });
  RunLoopUntilIdle();

  ASSERT_FALSE(server.HasActiveWakeLease());
}

}  // namespace
