// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "low_energy_connection_server.h"

#include "src/connectivity/bluetooth/core/bt-host/fidl/adapter_test_fixture.h"

namespace bthost {
namespace {

namespace fble = fuchsia::bluetooth::le;
namespace fbg = fuchsia::bluetooth::gatt2;

const bt::DeviceAddress kTestAddr(bt::DeviceAddress::Type::kLEPublic, {0x01, 0, 0, 0, 0, 0});

class LowEnergyConnectionServerTest : public bthost::testing::AdapterTestFixture {
 public:
  LowEnergyConnectionServerTest() = default;
  ~LowEnergyConnectionServerTest() override = default;

  void SetUp() override {
    bthost::testing::AdapterTestFixture::SetUp();

    fidl::InterfaceHandle<fuchsia::bluetooth::le::Connection> handle;
    std::unique_ptr<bt::gap::LowEnergyConnectionHandle> connection = EstablishConnection();
    server_ = std::make_unique<LowEnergyConnectionServer>(
        adapter()->AsWeakPtr(), gatt()->GetWeakPtr(), std::move(connection),
        handle.NewRequest().TakeChannel(),
        /*closed_cb=*/[this] {
          server_closed_cb_called_ = true;
          server_.reset();
        });
    client_ = handle.Bind();
  }

  fble::Connection* client() { return client_.get(); }

  void UnbindClient() { client_.Unbind(); }

  bt::PeerId peer_id() const { return peer_id_; }

  bool server_closed_cb_called() const { return server_closed_cb_called_; }

 protected:
  void RunGetCodecDelayRangeTest(
      fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest&& params,
      std::optional<zx_status_t> err);

 private:
  std::unique_ptr<bt::gap::LowEnergyConnectionHandle> EstablishConnection() {
    // Since LowEnergyConnectionHandle must be created by LowEnergyConnectionManager, we discover
    // and connect to a fake peer to get a LowEnergyConnectionHandle.
    std::unique_ptr<bt::testing::FakePeer> fake_peer =
        std::make_unique<bt::testing::FakePeer>(kTestAddr, pw_dispatcher(), /*connectable=*/true);
    test_device()->AddPeer(std::move(fake_peer));

    std::optional<bt::PeerId> peer_id;
    bt::gap::LowEnergyDiscoverySessionPtr session;
    adapter()->le()->StartDiscovery(
        /*active=*/true, [&](bt::gap::LowEnergyDiscoverySessionPtr cb_session) {
          session = std::move(cb_session);
          session->SetResultCallback(
              [&](const bt::gap::Peer& peer) { peer_id = peer.identifier(); });
        });
    RunLoopUntilIdle();
    BT_ASSERT(peer_id);
    peer_id_ = *peer_id;

    std::optional<bt::gap::LowEnergyConnectionManager::ConnectionResult> conn_result;
    adapter()->le()->Connect(
        peer_id_,
        [&](bt::gap::LowEnergyConnectionManager::ConnectionResult result) {
          conn_result = std::move(result);
        },
        bt::gap::LowEnergyConnectionOptions());
    RunLoopUntilIdle();
    BT_ASSERT(conn_result);
    BT_ASSERT(conn_result->is_ok());
    return std::move(*conn_result).value();
  }

  std::unique_ptr<LowEnergyConnectionServer> server_;
  fble::ConnectionPtr client_;
  bool server_closed_cb_called_ = false;
  bt::PeerId peer_id_;
};

TEST_F(LowEnergyConnectionServerTest, RequestGattClientTwice) {
  fidl::InterfaceHandle<fuchsia::bluetooth::gatt2::Client> handle_0;
  client()->RequestGattClient(handle_0.NewRequest());
  fbg::ClientPtr client_0 = handle_0.Bind();
  std::optional<zx_status_t> client_epitaph_0;
  client_0.set_error_handler([&](zx_status_t epitaph) { client_epitaph_0 = epitaph; });
  RunLoopUntilIdle();
  EXPECT_FALSE(client_epitaph_0);

  fidl::InterfaceHandle<fuchsia::bluetooth::gatt2::Client> handle_1;
  client()->RequestGattClient(handle_1.NewRequest());
  fbg::ClientPtr client_1 = handle_1.Bind();
  std::optional<zx_status_t> client_epitaph_1;
  client_1.set_error_handler([&](zx_status_t epitaph) { client_epitaph_1 = epitaph; });
  RunLoopUntilIdle();
  EXPECT_FALSE(client_epitaph_0);
  ASSERT_TRUE(client_epitaph_1);
  EXPECT_EQ(client_epitaph_1.value(), ZX_ERR_ALREADY_BOUND);
}

TEST_F(LowEnergyConnectionServerTest, GattClientServerError) {
  fidl::InterfaceHandle<fuchsia::bluetooth::gatt2::Client> handle_0;
  client()->RequestGattClient(handle_0.NewRequest());
  fbg::ClientPtr client_0 = handle_0.Bind();
  std::optional<zx_status_t> client_epitaph_0;
  client_0.set_error_handler([&](zx_status_t epitaph) { client_epitaph_0 = epitaph; });
  RunLoopUntilIdle();
  EXPECT_FALSE(client_epitaph_0);

  // Calling WatchServices twice should cause a server error.
  client_0->WatchServices({}, [](auto, auto) {});
  client_0->WatchServices({}, [](auto, auto) {});
  RunLoopUntilIdle();
  EXPECT_TRUE(client_epitaph_0);

  // Requesting a new GATT client should succeed.
  fidl::InterfaceHandle<fuchsia::bluetooth::gatt2::Client> handle_1;
  client()->RequestGattClient(handle_1.NewRequest());
  fbg::ClientPtr client_1 = handle_1.Bind();
  std::optional<zx_status_t> client_epitaph_1;
  client_1.set_error_handler([&](zx_status_t epitaph) { client_epitaph_1 = epitaph; });
  RunLoopUntilIdle();
  EXPECT_FALSE(client_epitaph_1);
}

TEST_F(LowEnergyConnectionServerTest, RequestGattClientThenUnbindThenRequestAgainShouldSucceed) {
  fidl::InterfaceHandle<fuchsia::bluetooth::gatt2::Client> handle_0;
  client()->RequestGattClient(handle_0.NewRequest());
  fbg::ClientPtr client_0 = handle_0.Bind();
  std::optional<zx_status_t> client_epitaph_0;
  client_0.set_error_handler([&](zx_status_t epitaph) { client_epitaph_0 = epitaph; });
  RunLoopUntilIdle();
  EXPECT_FALSE(client_epitaph_0);
  client_0.Unbind();
  RunLoopUntilIdle();

  // Requesting a new GATT client should succeed.
  fidl::InterfaceHandle<fuchsia::bluetooth::gatt2::Client> handle_1;
  client()->RequestGattClient(handle_1.NewRequest());
  fbg::ClientPtr client_1 = handle_1.Bind();
  std::optional<zx_status_t> client_epitaph_1;
  client_1.set_error_handler([&](zx_status_t epitaph) { client_epitaph_1 = epitaph; });
  RunLoopUntilIdle();
  EXPECT_FALSE(client_epitaph_1);
}

static ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest
CreateDelayRangeRequestParams(bool has_vendor_config) {
  ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest params;

  // params.logical_transport_type
  params.set_logical_transport_type(fuchsia::bluetooth::LogicalTransportType::LE_CIS);

  // params.data_direction
  params.set_data_direction(fuchsia::bluetooth::DataDirection::INPUT);

  // params.codec_attributes
  if (has_vendor_config) {
    uint16_t kCompanyId = 0x1234;
    uint16_t kVendorId = 0xfedc;
    fuchsia::bluetooth::VendorCodingFormat vendor_coding_format;
    vendor_coding_format.set_company_id(kCompanyId);
    vendor_coding_format.set_vendor_id(kVendorId);
    fuchsia::bluetooth::CodecAttributes codec_attributes;
    codec_attributes.mutable_codec_id()->set_vendor_format(std::move(vendor_coding_format));
    std::vector<uint8_t> codec_configuration{0x4f, 0x77, 0x65, 0x6e};
    codec_attributes.set_codec_configuration(codec_configuration);
    params.set_codec_attributes(std::move(codec_attributes));
  } else {
    fuchsia::bluetooth::CodecAttributes codec_attributes;
    codec_attributes.mutable_codec_id()->set_assigned_format(
        fuchsia::bluetooth::AssignedCodingFormat::LINEAR_PCM);
    params.set_codec_attributes(std::move(codec_attributes));
  }

  return params;
}

void LowEnergyConnectionServerTest::RunGetCodecDelayRangeTest(
    ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest&& params,
    std::optional<zx_status_t> err = std::nullopt) {
  fuchsia::bluetooth::le::CodecDelay_GetCodecLocalDelayRange_Result result;
  LowEnergyConnectionServer::GetCodecLocalDelayRangeCallback callback =
      [&](fuchsia::bluetooth::le::CodecDelay_GetCodecLocalDelayRange_Result cb_result) {
        result = std::move(cb_result);
      };
  client()->GetCodecLocalDelayRange(std::move(params), std::move(callback));
  RunLoopUntilIdle();
  if (err.has_value()) {
    ASSERT_TRUE(result.is_err());
    EXPECT_EQ(result.err(), err.value());
  } else {
    ASSERT_TRUE(result.is_response());
    auto& response = result.response();
    // These are the default values returned by the FakeController
    EXPECT_EQ(response.min_controller_delay(), zx::sec(0).get());
    EXPECT_EQ(response.max_controller_delay(), zx::sec(4).get());
  }
}

// Invoking GetCodecLocalDelay with a spec-defined coding format
TEST_F(LowEnergyConnectionServerTest, GetCodecLocalDelaySpecCodingFormat) {
  ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest params =
      CreateDelayRangeRequestParams(/*has_vendor_config=*/false);
  RunGetCodecDelayRangeTest(std::move(params));
}

// Invoking GetCodecLocalDelay with a vendor-defined coding format
TEST_F(LowEnergyConnectionServerTest, GetCodecLocalDelayVendorCodingFormat) {
  ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest params =
      CreateDelayRangeRequestParams(/*has_vendor_config=*/true);
  RunGetCodecDelayRangeTest(std::move(params));
}

// Invoking GetCodecLocalDelay with missing parameters
TEST_F(LowEnergyConnectionServerTest, GetCodecLocalDelayMissingParams) {
  // Logical transport type missing
  ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest params =
      CreateDelayRangeRequestParams(/*has_vendor_config=*/false);
  params.clear_logical_transport_type();
  RunGetCodecDelayRangeTest(std::move(params), ZX_ERR_INVALID_ARGS);

  // Data direction is missing
  params = CreateDelayRangeRequestParams(/*has_vendor_config=*/false);
  params.clear_data_direction();
  RunGetCodecDelayRangeTest(std::move(params), ZX_ERR_INVALID_ARGS);

  // Codec attributes is missing
  params = CreateDelayRangeRequestParams(/*has_vendor_config=*/true);
  params.clear_codec_attributes();
  RunGetCodecDelayRangeTest(std::move(params), ZX_ERR_INVALID_ARGS);

  // codec_attributes.codec_id is missing
  params = CreateDelayRangeRequestParams(/*has_vendor_config=*/true);
  params.mutable_codec_attributes()->clear_codec_id();
  RunGetCodecDelayRangeTest(std::move(params), ZX_ERR_INVALID_ARGS);
}

// Calling GetCodecLocalDelay when the controller doesn't support it
TEST_F(LowEnergyConnectionServerTest, GetCodecLocalDelayCommandNotSupported) {
  // Disable the Read Local Supported Controller Delay instruction
  bt::testing::FakeController::Settings settings;
  constexpr size_t kReadLocalSupportedControllerDelayOctet = 45;
  settings.supported_commands[kReadLocalSupportedControllerDelayOctet] &=
      ~static_cast<uint8_t>(bt::hci_spec::SupportedCommand::kReadLocalSupportedControllerDelay);
  test_device()->set_settings(settings);

  ::fuchsia::bluetooth::le::CodecDelayGetCodecLocalDelayRangeRequest params =
      CreateDelayRangeRequestParams(/*has_vendor_config=*/false);
  RunGetCodecDelayRangeTest(std::move(params), ZX_ERR_INTERNAL);
}

TEST_F(LowEnergyConnectionServerTest, ServerClosedOnConnectionClosed) {
  adapter()->le()->Disconnect(peer_id());
  RunLoopUntilIdle();
  EXPECT_TRUE(server_closed_cb_called());
}

TEST_F(LowEnergyConnectionServerTest, ServerClosedWhenFIDLClientClosesConnection) {
  UnbindClient();
  RunLoopUntilIdle();
  EXPECT_TRUE(server_closed_cb_called());
}

}  // namespace
}  // namespace bthost
