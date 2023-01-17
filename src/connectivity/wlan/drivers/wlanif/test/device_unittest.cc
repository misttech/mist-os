// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/wlanif/device.h"

#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl_test_base.h>
#include <fuchsia/wlan/sme/cpp/fidl.h>
#include <fuchsia/wlan/sme/cpp/fidl_test_base.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/decoder.h>
#include <lib/fidl/cpp/message.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <zircon/errors.h>

#include <functional>
#include <memory>
#include <new>
#include <optional>
#include <tuple>
#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "fuchsia/hardware/wlan/fullmac/c/banjo.h"
#include "fuchsia/wlan/common/c/banjo.h"
#include "fuchsia/wlan/common/cpp/fidl.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "test_bss.h"

namespace {

namespace wlan_mlme = ::fuchsia::wlan::mlme;
namespace wlan_common = ::fuchsia::wlan::common;

using ::testing::_;
using ::testing::ElementsAre;

bool multicast_promisc_enabled = false;

static zx_status_t hook_set_multicast_promisc(void* ctx, bool enable) {
  multicast_promisc_enabled = enable;
  return ZX_OK;
}

std::pair<zx::channel, zx::channel> make_channel() {
  zx::channel local;
  zx::channel remote;
  zx::channel::create(0, &local, &remote);
  return {std::move(local), std::move(remote)};
}

// Verify that receiving an ethernet SetParam for multicast promiscuous mode results in a call to
// wlan_fullmac_impl->set_muilticast_promisc.
TEST(MulticastPromiscMode, OnOff) {
  zx_status_t status;

  wlan_fullmac_impl_protocol_ops_t proto_ops = {.set_multicast_promisc =
                                                    hook_set_multicast_promisc};
  wlan_fullmac_impl_protocol_t proto = {.ops = &proto_ops};
  wlanif::Device device(nullptr, proto);

  multicast_promisc_enabled = false;

  // Disable => Enable
  status = device.EthSetParam(ETHERNET_SETPARAM_MULTICAST_PROMISC, 1, nullptr, 0);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_EQ(multicast_promisc_enabled, true);

  // Enable => Enable
  status = device.EthSetParam(ETHERNET_SETPARAM_MULTICAST_PROMISC, 1, nullptr, 0);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_EQ(multicast_promisc_enabled, true);

  // Enable => Enable (any non-zero value should be treated as "true")
  status = device.EthSetParam(ETHERNET_SETPARAM_MULTICAST_PROMISC, 0x80, nullptr, 0);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_EQ(multicast_promisc_enabled, true);

  // Enable => Disable
  status = device.EthSetParam(ETHERNET_SETPARAM_MULTICAST_PROMISC, 0, nullptr, 0);
  EXPECT_EQ(status, ZX_OK);
  EXPECT_EQ(multicast_promisc_enabled, false);
}

// Verify that we get a ZX_ERR_UNSUPPORTED back if the set_multicast_promisc hook is unimplemented.
TEST(MulticastPromiscMode, Unimplemented) {
  zx_status_t status;

  wlan_fullmac_impl_protocol_ops_t proto_ops = {};
  wlan_fullmac_impl_protocol_t proto = {.ops = &proto_ops};
  wlanif::Device device(nullptr, proto);

  multicast_promisc_enabled = false;

  status = device.EthSetParam(ETHERNET_SETPARAM_MULTICAST_PROMISC, 1, nullptr, 0);
  EXPECT_EQ(status, ZX_ERR_NOT_SUPPORTED);
  EXPECT_EQ(multicast_promisc_enabled, false);
}

struct SmeChannelTestContext {
  SmeChannelTestContext() {
    auto [new_sme, new_mlme] = make_channel();
    mlme = std::move(new_mlme);
    sme = std::move(new_sme);
  }

  ~SmeChannelTestContext() {
    auto scan_req = this->scan_req;
    if (scan_req.has_value()) {
      if (scan_req->channels_list != nullptr) {
        delete[] const_cast<uint8_t*>(scan_req->channels_list);
      }
      if (scan_req->ssids_list != nullptr) {
        delete[] const_cast<cssid_t*>(scan_req->ssids_list);
      }
    }
  }

  zx::channel mlme = {};
  zx::channel sme = {};
  std::optional<wlan_fullmac_scan_req_t> scan_req = {};
};

wlan_fullmac_impl_protocol_ops_t EmptyProtoOps() {
  return wlan_fullmac_impl_protocol_ops_t{
      // Each instance is required to provide its own .start() method to store the MLME channels.
      // SME Channel will be provided to wlanif-impl-driver when it calls back into its parent.
      .query = [](void* ctx, wlan_fullmac_query_info_t* info) { memset(info, 0, sizeof(*info)); },
      .query_mac_sublayer_support =
          [](void* ctx, mac_sublayer_support_t* resp) { memset(resp, 0, sizeof(*resp)); },
      .query_security_support = [](void* ctx,
                                   security_support_t* resp) { memset(resp, 0, sizeof(*resp)); },
      .query_spectrum_management_support =
          [](void* ctx, spectrum_management_support_t* resp) { memset(resp, 0, sizeof(*resp)); },
      .start_scan = [](void* ctx, const wlan_fullmac_scan_req_t* req) {},
      .connect_req = [](void* ctx, const wlan_fullmac_connect_req_t* req) {},
      .reconnect_req = [](void* ctx, const wlan_fullmac_reconnect_req_t* req) {},
      .auth_resp = [](void* ctx, const wlan_fullmac_auth_resp_t* req) {},
      .deauth_req = [](void* ctx, const wlan_fullmac_deauth_req_t* req) {},
      .assoc_resp = [](void* ctx, const wlan_fullmac_assoc_resp_t* req) {},
      .disassoc_req = [](void* ctx, const wlan_fullmac_disassoc_req_t* req) {},
      .reset_req = [](void* ctx, const wlan_fullmac_reset_req_t* req) {},
      .start_req = [](void* ctx, const wlan_fullmac_start_req_t* req) {},
      .stop_req = [](void* ctx, const wlan_fullmac_stop_req_t* req) {},
      .set_keys_req = [](void* ctx, const wlan_fullmac_set_keys_req_t* req,
                         wlan_fullmac_set_keys_resp_t* resp) {},
      .del_keys_req = [](void* ctx, const wlan_fullmac_del_keys_req_t* req) {},
      .eapol_req = [](void* ctx, const wlan_fullmac_eapol_req_t* req) {},
  };
}

struct ConnectReqTestContext {
  ConnectReqTestContext() {
    auto [new_sme, new_mlme] = make_channel();
    mlme = std::move(new_mlme);
    sme = std::move(new_sme);
  }

  zx::channel mlme = {};
  zx::channel sme = {};
  std::optional<wlan_fullmac_connect_req_t> connect_req = {};
  wlan_fullmac_impl_ifc_protocol_t ifc = {};
  volatile std::atomic<bool> connect_received = false;
  volatile std::atomic<bool> connect_confirmed = false;
  volatile std::atomic<bool> ignore_connect = false;
};

struct DeviceTestFixture : public ::gtest::TestLoopFixture {
  void InitDevice();
  void TearDown() override {
    device_->Unbind();
    TestLoopFixture::TearDown();
  }
  zx_status_t HookStart(const wlan_fullmac_impl_ifc_protocol_t* ifc,
                        zx_handle_t* out_usme_bootstrap_channel) {
    auto [usme_bootstrap_client, usme_bootstrap_server] = make_channel();
    zx_status_t status = usme_bootstrap_.Bind(std::move(usme_bootstrap_client), dispatcher());
    if (status != ZX_OK) {
      return status;
    }

    auto [generic_sme_client, generic_sme_server_chan] = make_channel();
    status = generic_sme_.Bind(std::move(usme_bootstrap_client), dispatcher());
    if (status != ZX_OK) {
      return status;
    }
    auto generic_sme_server =
        fidl::InterfaceRequest<fuchsia::wlan::sme::GenericSme>(std::move(generic_sme_server_chan));

    fuchsia::wlan::sme::LegacyPrivacySupport legacy_privacy_support{
        .wep_supported = false,
        .wpa1_supported = false,
    };
    usme_bootstrap_->Start(std::move(generic_sme_server), legacy_privacy_support);

    wlan_fullmac_impl_ifc_ = *ifc;
    *out_usme_bootstrap_channel = usme_bootstrap_server.release();
    return ZX_OK;
  }
  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();
  wlan_fullmac_impl_protocol_ops_t proto_ops_ = EmptyProtoOps();
  wlan_fullmac_impl_protocol_t proto_{.ops = &proto_ops_, .ctx = this};
  // The parent calls release on this pointer which will delete it so don't delete it or manage it.
  wlanif::Device* device_{new wlanif::Device(parent_.get(), proto_)};
  wlan_fullmac_impl_ifc_protocol_t wlan_fullmac_impl_ifc_{};

  fuchsia::wlan::sme::UsmeBootstrapPtr usme_bootstrap_;
  fuchsia::wlan::sme::GenericSmePtr generic_sme_;
};

#define DEV(c) static_cast<DeviceTestFixture*>(c)
static zx_status_t hook_start(void* ctx, const wlan_fullmac_impl_ifc_protocol_t* ifc,
                              zx_handle_t* usme_bootstrap_channel) {
  return DEV(ctx)->HookStart(ifc, usme_bootstrap_channel);
}
#undef DEV

void DeviceTestFixture::InitDevice() {
  proto_ops_.start = hook_start;
  ASSERT_EQ(device_->Bind(), ZX_OK);
}

struct EthernetTestFixture : public DeviceTestFixture {
  void InitDeviceWithRole(wlan_mac_role_t role);
  void SetEthernetOnline(uint32_t expected_status = ETHERNET_STATUS_ONLINE) {
    const bool online = true;
    device_->OnLinkStateChanged(online);
    ASSERT_EQ(ethernet_status_, expected_status);
  }
  void SetEthernetOffline(uint32_t expected_status = 0) {
    const bool online = false;
    device_->OnLinkStateChanged(online);
    ASSERT_EQ(ethernet_status_, expected_status);
  }
  void CallDataRecv() {
    // Doesn't matter what we put in as argument here since we just want to test for deadlock.
    device_->EthRecv(nullptr, 0, 0);
  }

  ethernet_ifc_protocol_ops_t eth_ops_{};
  ethernet_ifc_protocol_t eth_proto_ = {.ops = &eth_ops_, .ctx = this};
  wlan_mac_role_t role_ = WLAN_MAC_ROLE_CLIENT;
  uint32_t ethernet_status_{0};
  data_plane_type_t data_plane_type_ = DATA_PLANE_TYPE_ETHERNET_DEVICE;
  std::optional<bool> link_state_;
  std::function<void(const wlan_fullmac_start_req_t*)> start_req_cb_;

  volatile std::atomic<bool> eth_recv_called_ = false;
};

#define ETH_DEV(c) static_cast<EthernetTestFixture*>(c)
static void hook_query(void* ctx, wlan_fullmac_query_info_t* info) {
  info->role = ETH_DEV(ctx)->role_;
  info->band_cap_count = 0;
}

static void hook_query_mac_sublayer_support(void* ctx, mac_sublayer_support_t* out_resp) {
  out_resp->device.mac_implementation_type = MAC_IMPLEMENTATION_TYPE_FULLMAC;
  out_resp->data_plane.data_plane_type = ETH_DEV(ctx)->data_plane_type_;
}

static void hook_eth_status(void* ctx, uint32_t status) { ETH_DEV(ctx)->ethernet_status_ = status; }
static void hook_eth_recv(void* ctx, const uint8_t* buffer, size_t data_size, uint32_t flags) {
  ETH_DEV(ctx)->eth_recv_called_ = true;
}

static void hook_start_req(void* ctx, const wlan_fullmac_start_req_t* req) {
  if (ETH_DEV(ctx)->start_req_cb_) {
    ETH_DEV(ctx)->start_req_cb_(req);
  }
}

void EthernetTestFixture::InitDeviceWithRole(wlan_mac_role_t role) {
  role_ = role;
  proto_ops_.start = hook_start;
  proto_ops_.query = hook_query;
  proto_ops_.query_mac_sublayer_support = hook_query_mac_sublayer_support;
  proto_ops_.start_req = hook_start_req;
  eth_ops_.status = hook_eth_status;
  eth_ops_.recv = hook_eth_recv;
  ASSERT_EQ(device_->Bind(), ZX_OK);
}

TEST_F(EthernetTestFixture, ApIfaceHasApEthernetFeature) {
  InitDeviceWithRole(WLAN_MAC_ROLE_AP);
  ethernet_info_t info;
  device_->EthQuery(0, &info);
  ASSERT_TRUE(info.features & ETHERNET_FEATURE_WLAN);
  ASSERT_TRUE(info.features & ETHERNET_FEATURE_WLAN_AP);
}

TEST_F(EthernetTestFixture, StartThenSetOnline) {
  InitDeviceWithRole(WLAN_MAC_ROLE_AP);  // role doesn't matter
  device_->EthStart(&eth_proto_);
  ASSERT_EQ(ethernet_status_, 0u);
  SetEthernetOnline();
  ASSERT_EQ(ethernet_status_, ETHERNET_STATUS_ONLINE);
}

TEST_F(EthernetTestFixture, OnlineThenStart) {
  InitDeviceWithRole(WLAN_MAC_ROLE_AP);  // role doesn't matter
  SetEthernetOnline(0);
  ASSERT_EQ(ethernet_status_, 0u);
  device_->EthStart(&eth_proto_);
  ASSERT_EQ(ethernet_status_, ETHERNET_STATUS_ONLINE);
}

TEST_F(EthernetTestFixture, EthernetDataPlane) {
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);

  // The device added should support the ethernet impl protocol
  auto children = parent_->children();
  ASSERT_EQ(children.size(), 1u);
  ethernet_impl_protocol_t eth_impl_proto;
  EXPECT_EQ(device_get_protocol(children.front().get(), ZX_PROTOCOL_ETHERNET_IMPL, &eth_impl_proto),
            ZX_OK);
}

TEST_F(EthernetTestFixture, GndDataPlane) {
  data_plane_type_ = DATA_PLANE_TYPE_GENERIC_NETWORK_DEVICE;
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);

  // The device added should NOT support the ethernet impl protocol
  auto children = parent_->children();
  ASSERT_EQ(children.size(), 1u);
  ethernet_impl_protocol_t eth_impl_proto;
  EXPECT_NE(device_get_protocol(children.front().get(), ZX_PROTOCOL_ETHERNET_IMPL, &eth_impl_proto),
            ZX_OK);
}

TEST_F(EthernetTestFixture, NotifyOnline) {
  proto_ops_.on_link_state_changed = [](void* ctx, bool online) {
    reinterpret_cast<EthernetTestFixture*>(ctx)->link_state_ = online;
  };

  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  // Setting the device to online should result in a link state change.
  SetEthernetOnline();
  ASSERT_TRUE(link_state_.has_value());
  EXPECT_TRUE(link_state_.value());

  // Clear the optional and then set the status to online again, another link state event should
  // not be sent.
  link_state_.reset();
  SetEthernetOnline();
  EXPECT_FALSE(link_state_.has_value());

  // Now set it to offline and verify we get a link state change.
  link_state_.reset();
  SetEthernetOffline();
  ASSERT_TRUE(link_state_.has_value());
  EXPECT_FALSE(link_state_.value());

  // And similarly setting it to offline when it's already offline should NOT send a link state
  // event.
  link_state_.reset();
  SetEthernetOffline();
  EXPECT_FALSE(link_state_.has_value());
}

TEST_F(EthernetTestFixture, GetIfaceCounterStatsReqDoesNotDeadlockWithEthRecv) {
  proto_ops_.get_iface_counter_stats =
      [](void* ctx, wlan_fullmac_iface_counter_stats_t* out_stats) -> int32_t {
    ETH_DEV(ctx)->CallDataRecv();
    return ZX_OK;
  };
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  wlan_fullmac_iface_counter_stats_t out_stats;
  device_->GetIfaceCounterStats(&out_stats);
  ASSERT_TRUE(eth_recv_called_);
}

TEST_F(EthernetTestFixture, GetIfaceHistogramStatsReqDoesNotDeadlockWithEthRecv) {
  proto_ops_.get_iface_histogram_stats =
      [](void* ctx, wlan_fullmac_iface_histogram_stats_t* out_stats) -> int32_t {
    ETH_DEV(ctx)->CallDataRecv();
    return ZX_OK;
  };
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  wlan_fullmac_iface_histogram_stats_t out_stats;
  device_->GetIfaceHistogramStats(&out_stats);
  ASSERT_TRUE(eth_recv_called_);
}

TEST_F(EthernetTestFixture, ConnectReqDoesNotDeadlockWithEthRecv) {
  proto_ops_.connect_req = [](void* ctx, const wlan_fullmac_connect_req_t* req) {
    ETH_DEV(ctx)->CallDataRecv();
  };
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  wlan_fullmac_connect_req_t req;
  device_->ConnectReq(&req);
  ASSERT_TRUE(eth_recv_called_);
}

TEST_F(EthernetTestFixture, DeauthReqDoesNotDeadlockWithEthRecv) {
  proto_ops_.deauth_req = [](void* ctx, const wlan_fullmac_deauth_req_t* req) {
    ETH_DEV(ctx)->CallDataRecv();
  };
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  wlan_fullmac_deauth_req_t req;
  device_->DeauthenticateReq(&req);
  ASSERT_TRUE(eth_recv_called_);
}

TEST_F(EthernetTestFixture, DisassociateReqDoesNotDeadlockWithEthRecv) {
  proto_ops_.disassoc_req = [](void* ctx, const wlan_fullmac_disassoc_req_t* req) {
    ETH_DEV(ctx)->CallDataRecv();
  };
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  wlan_fullmac_disassoc_req_t disassoc_req;
  device_->DisassociateReq(&disassoc_req);
  ASSERT_TRUE(eth_recv_called_);
}

TEST_F(EthernetTestFixture, StartReqDoesNotDeadlockWithEthRecv) {
  start_req_cb_ = [this](const wlan_fullmac_start_req_t* req) { this->CallDataRecv(); };
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  wlan_fullmac_start_req_t start_req;
  device_->StartReq(&start_req);
  ASSERT_TRUE(eth_recv_called_);
}

TEST_F(EthernetTestFixture, StopReqDoesNotDeadlockWithEthRecv) {
  proto_ops_.stop_req = [](void* ctx, const wlan_fullmac_stop_req_t* req) {
    ETH_DEV(ctx)->CallDataRecv();
  };
  InitDeviceWithRole(WLAN_MAC_ROLE_CLIENT);
  device_->EthStart(&eth_proto_);

  wlan_fullmac_stop_req_t stop_req;
  device_->StopReq(&stop_req);
  ASSERT_TRUE(eth_recv_called_);
}

#undef ETH_DEV

}  // namespace
