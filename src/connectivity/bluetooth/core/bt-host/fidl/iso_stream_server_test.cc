// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "iso_stream_server.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/loop_fixture.h"

namespace bthost {
namespace {

const bt::iso::CisEstablishedParameters kCisParameters = {
    .cig_sync_delay = 1000000,
    .cis_sync_delay = 2000000,
    .max_subevents = 5,
    .iso_interval = 15,
    .c_to_p_params =
        {
            .transport_latency = 5000,
            .phy = pw::bluetooth::emboss::IsoPhyType::LE_1M,
            .burst_number = 3,
            .flush_timeout = 100,
            .max_pdu_size = 120,
        },
    .p_to_c_params =
        {
            .transport_latency = 6000,
            .phy = pw::bluetooth::emboss::IsoPhyType::LE_CODED,
            .burst_number = 4,
            .flush_timeout = 60,
            .max_pdu_size = 70,
        },
};

using TestingBase = bt::testing::TestLoopFixture;
class IsoStreamServerTest : public TestingBase {
 public:
  IsoStreamServerTest() = default;
  ~IsoStreamServerTest() override = default;

  void SetUp() override {
    TestingBase::SetUp();

    fidl::InterfaceHandle<fuchsia::bluetooth::le::IsochronousStream> handle;
    server_ = std::make_unique<IsoStreamServer>(handle.NewRequest(), [this]() { OnClosed(); });
    client_.Bind(std::move(handle), dispatcher());
    client_.set_error_handler([this](zx_status_t status) { epitaph_ = status; });
    client_.events().OnEstablished =
        [this](::fuchsia::bluetooth::le::IsochronousStreamOnEstablishedRequest request) {
          on_established_events_.push(std::move(request));
        };
  }

  void TearDown() override {
    RunLoopUntilIdle();
    CloseProxy();
    server_ = nullptr;
    TestingBase::TearDown();
  }

 protected:
  void OnClosed() { on_closed_called_times_++; }
  void CloseProxy() { client_ = nullptr; }
  IsoStreamServer* server() const { return server_.get(); }
  std::optional<zx_status_t> epitaph() const { return epitaph_; }
  std::queue<::fuchsia::bluetooth::le::IsochronousStreamOnEstablishedRequest>
      on_established_events_;
  uint32_t on_closed_called_times_ = 0;

 private:
  std::unique_ptr<IsoStreamServer> server_;
  fuchsia::bluetooth::le::IsochronousStreamPtr client_;
  std::optional<zx_status_t> epitaph_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(IsoStreamServerTest);
};

TEST_F(IsoStreamServerTest, ClosedServerSide) {
  server()->Close(ZX_ERR_WRONG_TYPE);
  RunLoopUntilIdle();
  auto status = epitaph();
  ASSERT_TRUE(status);
  EXPECT_EQ(*status, ZX_ERR_WRONG_TYPE);
  EXPECT_EQ(on_closed_called_times_, 1u);
}

TEST_F(IsoStreamServerTest, ClosedClientSide) {
  CloseProxy();
  RunLoopUntilIdle();
  EXPECT_EQ(on_closed_called_times_, 1u);
}

// Verify that when an IsoStreamServer receives notification of a successful stream establishment
// it sends the stream parameters back to the client.
TEST_F(IsoStreamServerTest, StreamEstablishedSuccessfully) {
  EXPECT_EQ(on_established_events_.size(), (size_t)0);
  server()->OnStreamEstablished(pw::bluetooth::emboss::StatusCode::SUCCESS, kCisParameters);
  RunLoopUntilIdle();
  ASSERT_EQ(on_established_events_.size(), (size_t)1);

  auto& event = on_established_events_.front();
  ASSERT_TRUE(event.has_result());
  EXPECT_EQ(event.result(), ZX_OK);

  ASSERT_TRUE(event.has_established_params());
  auto& established_params = event.established_params();
  ASSERT_TRUE(established_params.has_cig_sync_delay());
  EXPECT_EQ(established_params.cig_sync_delay(), zx::usec(kCisParameters.cig_sync_delay).get());
  ASSERT_TRUE(established_params.has_cis_sync_delay());
  EXPECT_EQ(established_params.cis_sync_delay(), zx::usec(kCisParameters.cis_sync_delay).get());
  ASSERT_TRUE(established_params.has_max_subevents());
  EXPECT_EQ(established_params.max_subevents(), kCisParameters.max_subevents);
  ASSERT_TRUE(established_params.has_iso_interval());
  // Each increment represent 1.25ms
  EXPECT_EQ(established_params.iso_interval(), zx::usec(kCisParameters.iso_interval * 1250).get());

  ASSERT_TRUE(established_params.has_central_to_peripheral_params());
  auto& c_to_p_params = established_params.central_to_peripheral_params();
  ASSERT_TRUE(c_to_p_params.has_transport_latency());
  EXPECT_EQ(c_to_p_params.transport_latency(),
            zx::usec(kCisParameters.c_to_p_params.transport_latency).get());
  ASSERT_TRUE(c_to_p_params.has_burst_number());
  EXPECT_EQ(c_to_p_params.burst_number(), kCisParameters.c_to_p_params.burst_number);
  ASSERT_TRUE(c_to_p_params.has_flush_timeout());
  EXPECT_EQ(c_to_p_params.flush_timeout(), kCisParameters.c_to_p_params.flush_timeout);

  ASSERT_TRUE(established_params.has_peripheral_to_central_params());
  auto& p_to_c_params = established_params.peripheral_to_central_params();
  ASSERT_TRUE(p_to_c_params.has_transport_latency());
  EXPECT_EQ(p_to_c_params.transport_latency(),
            zx::usec(kCisParameters.p_to_c_params.transport_latency).get());
  ASSERT_TRUE(p_to_c_params.has_burst_number());
  EXPECT_EQ(p_to_c_params.burst_number(), kCisParameters.p_to_c_params.burst_number);
  ASSERT_TRUE(p_to_c_params.has_flush_timeout());
  EXPECT_EQ(p_to_c_params.flush_timeout(), kCisParameters.p_to_c_params.flush_timeout);
}

// Verify that on failure we properly notify the client, set status code to ZX_ERR_INTERNAL, and
// don't pass back any stream parameters.
TEST_F(IsoStreamServerTest, StreamNotEstablished) {
  EXPECT_EQ(on_established_events_.size(), 0u);
  server()->OnStreamEstablished(pw::bluetooth::emboss::StatusCode::UNSPECIFIED_ERROR, std::nullopt);
  RunLoopUntilIdle();
  ASSERT_EQ(on_established_events_.size(), 1u);
  auto& event1 = on_established_events_.front();
  ASSERT_TRUE(event1.has_result());
  EXPECT_EQ(event1.result(), ZX_ERR_INTERNAL);
  ASSERT_FALSE(event1.has_established_params());
  on_established_events_.pop();

  server()->OnStreamEstablished(pw::bluetooth::emboss::StatusCode::UNKNOWN_COMMAND, kCisParameters);
  RunLoopUntilIdle();
  ASSERT_EQ(on_established_events_.size(), 1u);
  auto& event2 = on_established_events_.front();
  ASSERT_TRUE(event2.has_result());
  EXPECT_EQ(event2.result(), ZX_ERR_INTERNAL);
  ASSERT_FALSE(event2.has_established_params());
  on_established_events_.pop();
}

}  // namespace
}  // namespace bthost
