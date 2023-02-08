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
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace {

std::pair<zx::channel, zx::channel> make_channel() {
  zx::channel local;
  zx::channel remote;
  zx::channel::create(0, &local, &remote);
  return {std::move(local), std::move(remote)};
}

struct BindTestFixture : public ::gtest::TestLoopFixture {
  void SetUp() override {
    proto_ops_ = wlan_fullmac_impl_protocol_ops_t{
        .start = [](void* ctx, const wlan_fullmac_impl_ifc_protocol_t* ifc,
                    zx_handle_t* usme_bootstrap_channel) -> zx_status_t {
          return static_cast<decltype(this)>(ctx)->HookStart(ifc, usme_bootstrap_channel);
        },
        .query =
            [](void* ctx, wlan_fullmac_query_info_t* info) {
              info->role = WLAN_MAC_ROLE_CLIENT;
              info->band_cap_count = 0;
            },
        .query_mac_sublayer_support =
            [](void* ctx, mac_sublayer_support_t* resp) {
              resp->device.mac_implementation_type = MAC_IMPLEMENTATION_TYPE_FULLMAC;
              resp->data_plane.data_plane_type = DATA_PLANE_TYPE_GENERIC_NETWORK_DEVICE;
            },
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
        .on_link_state_changed = [](void* ctx, bool) {},
    };
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
  wlan_fullmac_impl_protocol_ops_t proto_ops_{};
  wlan_fullmac_impl_protocol_t proto_{.ops = &proto_ops_, .ctx = this};

  // If the device binds successfully, the parent calls release on this pointer which will delete it
  // so don't delete it or manage it.
  wlanif::Device* device_{new wlanif::Device(parent_.get(), proto_)};
  wlan_fullmac_impl_ifc_protocol_t wlan_fullmac_impl_ifc_{};

  fuchsia::wlan::sme::UsmeBootstrapPtr usme_bootstrap_;
  fuchsia::wlan::sme::GenericSmePtr generic_sme_;
};

TEST_F(BindTestFixture, GndDataPlaneCanBind) {
  ASSERT_EQ(device_->Bind(), ZX_OK);

  // The device added should NOT support the ethernet impl protocol
  auto children = parent_->children();
  ASSERT_EQ(children.size(), 1u);
  ethernet_impl_protocol_t eth_impl_proto;
  EXPECT_NE(device_get_protocol(children.front().get(), ZX_PROTOCOL_ETHERNET_IMPL, &eth_impl_proto),
            ZX_OK);

  device_->Unbind();
}

TEST_F(BindTestFixture, EthDataPlaneNotSupported) {
  proto_ops_.query_mac_sublayer_support = [](void* ctx, mac_sublayer_support_t* resp) {
    resp->device.mac_implementation_type = MAC_IMPLEMENTATION_TYPE_FULLMAC;
    resp->data_plane.data_plane_type = DATA_PLANE_TYPE_ETHERNET_DEVICE;
  };

  ASSERT_EQ(device_->Bind(), ZX_ERR_NOT_SUPPORTED);

  // The device didn't bind because ethernet isn't supported, so we have to manually free the device
  device_->Release();
}

struct WlanifTestFixture : public BindTestFixture {
  void SetUp() override {
    BindTestFixture::SetUp();
    ASSERT_EQ(ZX_OK, device_->Bind());
  }

  void TearDown() override {
    device_->Unbind();
    BindTestFixture::TearDown();
  }
};

TEST_F(WlanifTestFixture, OnLinkStateChangedOnlyCalledWhenStateChanges) {
  constexpr auto expect_true = [](void*, bool online) { EXPECT_TRUE(online); };
  constexpr auto expect_false = [](void*, bool online) { EXPECT_FALSE(online); };
  constexpr auto expect_not_called = [](void*, bool) { EXPECT_TRUE(false); };

  proto_ops_.on_link_state_changed = expect_true;
  device_->OnLinkStateChanged(true);

  proto_ops_.on_link_state_changed = expect_not_called;
  device_->OnLinkStateChanged(true);

  proto_ops_.on_link_state_changed = expect_false;
  device_->OnLinkStateChanged(false);

  proto_ops_.on_link_state_changed = expect_not_called;
  device_->OnLinkStateChanged(false);

  proto_ops_.on_link_state_changed = expect_true;
  device_->OnLinkStateChanged(true);
}

// TODO(fxb/121450) Add unit tests for other functions in wlanif::Device
}  // namespace
