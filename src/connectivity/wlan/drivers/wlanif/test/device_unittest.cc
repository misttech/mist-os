// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/wlanif/device.h"

#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl_test_base.h>
#include <fuchsia/wlan/sme/cpp/fidl.h>
#include <fuchsia/wlan/sme/cpp/fidl_test_base.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/decoder.h>
#include <lib/fidl/cpp/message.h>
#include <lib/sync/cpp/completion.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <zircon/errors.h>
#include <zircon/system/ulib/async-default/include/lib/async/default.h>

#include <functional>
#include <memory>
#include <new>
#include <optional>
#include <tuple>
#include <vector>

#include <zxtest/zxtest.h>

#include "fuchsia/hardware/wlan/fullmac/c/banjo.h"
#include "fuchsia/wlan/common/c/banjo.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

std::pair<zx::channel, zx::channel> make_channel() {
  zx::channel local;
  zx::channel remote;
  zx::channel::create(0, &local, &remote);
  return {std::move(local), std::move(remote)};
}

struct WlanifDeviceTest : public ::zxtest::Test,
                          public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImpl> {
  void SetUp() override {
    auto endpoints_fullmac_impl = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImpl>();
    ASSERT_FALSE(endpoints_fullmac_impl.is_error());

    device_ = new wlanif::Device(parent_.get(), std::move(endpoints_fullmac_impl->client));

    auto server_dispatcher = fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher();
    fdf::BindServer(server_dispatcher->get(), std::move(endpoints_fullmac_impl->server), this);
  }

  void TearDown() override { mock_ddk::ReleaseFlaggedDevices(device_->zxdev()); }

  // WlanFullmacImpl implementations, dispatching FIDL requests from wlanif driver.
  void Start(StartRequestView request, fdf::Arena& arena,
             StartCompleter::Sync& completer) override {
    client_ =
        fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(std::move(request->ifc));

    // Create and send a mock channel up here since the sme_channel field is required in the reply
    // of Start().
    auto [local, remote] = make_channel();
    completer.buffer(arena).ReplySuccess(std::move(local));
  }
  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer) override {}
  void Query(fdf::Arena& arena, QueryCompleter::Sync& completer) override {}
  void QueryMacSublayerSupport(fdf::Arena& arena,
                               QueryMacSublayerSupportCompleter::Sync& completer) override {
    fuchsia_wlan_common::wire::MacSublayerSupport mac_sublayer_support;
    mac_sublayer_support.data_plane.data_plane_type = data_plane_type_;
    mac_sublayer_support.device.mac_implementation_type =
        fuchsia_wlan_common::wire::MacImplementationType::kFullmac;
    completer.buffer(arena).ReplySuccess(mac_sublayer_support);
  }
  void QuerySecuritySupport(fdf::Arena& arena,
                            QuerySecuritySupportCompleter::Sync& completer) override {}
  void QuerySpectrumManagementSupport(
      fdf::Arena& arena, QuerySpectrumManagementSupportCompleter::Sync& completer) override {}
  void StartScan(StartScanRequestView request, fdf::Arena& arena,
                 StartScanCompleter::Sync& completer) override {}
  void ConnectReq(ConnectReqRequestView request, fdf::Arena& arena,
                  ConnectReqCompleter::Sync& completer) override {}
  void ReconnectReq(ReconnectReqRequestView request, fdf::Arena& arena,
                    ReconnectReqCompleter::Sync& completer) override {}
  void AuthResp(AuthRespRequestView request, fdf::Arena& arena,
                AuthRespCompleter::Sync& completer) override {}
  void DeauthReq(DeauthReqRequestView request, fdf::Arena& arena,
                 DeauthReqCompleter::Sync& completer) override {}
  void AssocResp(AssocRespRequestView request, fdf::Arena& arena,
                 AssocRespCompleter::Sync& completer) override {}
  void DisassocReq(DisassocReqRequestView request, fdf::Arena& arena,
                   DisassocReqCompleter::Sync& completer) override {}
  void ResetReq(ResetReqRequestView request, fdf::Arena& arena,
                ResetReqCompleter::Sync& completer) override {}
  void StartReq(StartReqRequestView request, fdf::Arena& arena,
                StartReqCompleter::Sync& completer) override {}
  void StopReq(StopReqRequestView request, fdf::Arena& arena,
               StopReqCompleter::Sync& completer) override {}
  void SetKeysReq(SetKeysReqRequestView request, fdf::Arena& arena,
                  SetKeysReqCompleter::Sync& completer) override {}
  void DelKeysReq(DelKeysReqRequestView request, fdf::Arena& arena,
                  DelKeysReqCompleter::Sync& completer) override {}
  void EapolReq(EapolReqRequestView request, fdf::Arena& arena,
                EapolReqCompleter::Sync& completer) override {}
  void GetIfaceCounterStats(fdf::Arena& arena,
                            GetIfaceCounterStatsCompleter::Sync& completer) override {}
  void GetIfaceHistogramStats(fdf::Arena& arena,
                              GetIfaceHistogramStatsCompleter::Sync& completer) override {}
  void StartCaptureFrames(StartCaptureFramesRequestView request, fdf::Arena& arena,
                          StartCaptureFramesCompleter::Sync& completer) override {}
  void StopCaptureFrames(fdf::Arena& arena, StopCaptureFramesCompleter::Sync& completer) override {}
  void SetMulticastPromisc(SetMulticastPromiscRequestView request, fdf::Arena& arena,
                           SetMulticastPromiscCompleter::Sync& completer) override {}
  void SaeHandshakeResp(SaeHandshakeRespRequestView request, fdf::Arena& arena,
                        SaeHandshakeRespCompleter::Sync& completer) override {}
  void SaeFrameTx(SaeFrameTxRequestView request, fdf::Arena& arena,
                  SaeFrameTxCompleter::Sync& completer) override {}
  void WmmStatusReq(fdf::Arena& arena, WmmStatusReqCompleter::Sync& completer) override {}
  void OnLinkStateChanged(OnLinkStateChangedRequestView request, fdf::Arena& arena,
                          OnLinkStateChangedCompleter::Sync& completer) override {
    EXPECT_EQ(request->online, expected_online_);
    link_state_changed_called_ = true;
    completer.buffer(arena).Reply();
  }

  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();

  // Client to fire WlanFullmacImplIfc FIDL requests to wlanif driver.
  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc> client_;

  // If the device binds successfully, the parent calls release on this pointer which will delete it
  // so don't delete it or manage it.
  wlanif::Device* device_;

  fuchsia_wlan_common::wire::DataPlaneType data_plane_type_ =
      fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice;
  bool expected_online_ = false;
  bool link_state_changed_called_ = false;
};

TEST_F(WlanifDeviceTest, EthDataPlaneNotSupported) {
  data_plane_type_ = fuchsia_wlan_common::wire::DataPlaneType::kEthernetDevice;
  ASSERT_EQ(device_->Bind(), ZX_ERR_NOT_SUPPORTED);
  device_->AddDevice();
  device_->DdkAsyncRemove();
}

TEST_F(WlanifDeviceTest, OnLinkStateChangedOnlyCalledWhenStateChanges) {
  device_->AddDevice();
  device_->DdkAsyncRemove();

  expected_online_ = true;
  device_->OnLinkStateChanged(true);
  EXPECT_TRUE(link_state_changed_called_);

  link_state_changed_called_ = false;
  device_->OnLinkStateChanged(true);
  EXPECT_FALSE(link_state_changed_called_);

  link_state_changed_called_ = false;
  expected_online_ = false;
  device_->OnLinkStateChanged(false);
  EXPECT_TRUE(link_state_changed_called_);

  link_state_changed_called_ = false;
  device_->OnLinkStateChanged(false);
  EXPECT_FALSE(link_state_changed_called_);

  link_state_changed_called_ = false;
  expected_online_ = true;
  device_->OnLinkStateChanged(true);
  EXPECT_TRUE(link_state_changed_called_);
}

// TODO(fxb/121450) Add unit tests for other functions in wlanif::Device
}  // namespace
