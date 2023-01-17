// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fuchsia/hardware/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/stats/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/device.h>
#include <net/ethernet.h>
#include <zircon/status.h>

#include <memory>

#include <wlan/common/features.h>
#include <wlan/common/ieee80211_codes.h>
#include <wlan/drivers/log.h>

#include "debug.h"
#include "driver.h"
#include "fuchsia/wlan/common/c/banjo.h"
#include "fuchsia/wlan/common/cpp/fidl.h"
#include "zircon/system/public/zircon/assert.h"

namespace wlanif {

Device::Device(zx_device_t* device, wlan_fullmac_impl_protocol_t wlan_fullmac_impl_proto)
    : parent_(device), wlan_fullmac_impl_(wlan_fullmac_impl_proto) {
  ltrace_fn();
}

Device::~Device() { ltrace_fn(); }

#define DEV(c) static_cast<Device*>(c)
static zx_protocol_device_t device_ops = {
    .version = DEVICE_OPS_VERSION,
    .unbind = [](void* ctx) { DEV(ctx)->Unbind(); },
    .release = [](void* ctx) { DEV(ctx)->Release(); },
};

static ethernet_impl_protocol_ops_t ethernet_impl_ops = {
    .query = [](void* ctx, uint32_t options, ethernet_info_t* info) -> zx_status_t {
      return DEV(ctx)->EthQuery(options, info);
    },
    .stop = [](void* ctx) { DEV(ctx)->EthStop(); },
    .start = [](void* ctx, const ethernet_ifc_protocol_t* ifc) -> zx_status_t {
      return DEV(ctx)->EthStart(ifc);
    },
    .queue_tx =
        [](void* ctx, uint32_t options, ethernet_netbuf_t* netbuf,
           ethernet_impl_queue_tx_callback completion_cb,
           void* cookie) { return DEV(ctx)->EthQueueTx(options, netbuf, completion_cb, cookie); },
    .set_param = [](void* ctx, uint32_t param, int32_t value, const uint8_t* data, size_t data_size)
        -> zx_status_t { return DEV(ctx)->EthSetParam(param, value, data, data_size); },
};
#undef DEV

zx_status_t Device::AddDevice() {
  device_add_args_t args = {};
  args.version = DEVICE_ADD_ARGS_VERSION;
  args.name = "wlanif";
  args.ctx = this;
  args.ops = &device_ops;
  auto vmo = mlme_->DuplicateInspectVmo();
  if (vmo) {
    args.inspect_vmo = *vmo;
  }
  mac_sublayer_support mac_sublayer_support;
  wlan_fullmac_impl_query_mac_sublayer_support(&wlan_fullmac_impl_, &mac_sublayer_support);
  if (mac_sublayer_support.data_plane.data_plane_type == DATA_PLANE_TYPE_ETHERNET_DEVICE) {
    // This is an ethernet driver, add the custom protocol to the device
    args.proto_id = ZX_PROTOCOL_ETHERNET_IMPL;
    args.proto_ops = &ethernet_impl_ops;
  }
  return device_add(parent_, &args, &device_);
}

#define VERIFY_IMPL_PROTO_OP(fn)                                \
  do {                                                          \
    if (wlan_fullmac_impl_.ops->fn == nullptr) {                \
      lerror("required impl proto function %s missing\n", #fn); \
      return ZX_ERR_INVALID_ARGS;                               \
    }                                                           \
  } while (0)

zx_status_t Device::Bind() {
  ltrace_fn();

  // Assert minimum required functionality from the wlan_fullmac_impl driver
  if (wlan_fullmac_impl_.ops == nullptr) {
    lerror("no wlan_fullmac_impl protocol ops provided\n");
    return ZX_ERR_INVALID_ARGS;
  }

  VERIFY_IMPL_PROTO_OP(start);
  VERIFY_IMPL_PROTO_OP(query);
  VERIFY_IMPL_PROTO_OP(query_mac_sublayer_support);
  VERIFY_IMPL_PROTO_OP(query_security_support);
  VERIFY_IMPL_PROTO_OP(query_spectrum_management_support);
  VERIFY_IMPL_PROTO_OP(start_scan);
  VERIFY_IMPL_PROTO_OP(connect_req);
  VERIFY_IMPL_PROTO_OP(reconnect_req);
  VERIFY_IMPL_PROTO_OP(auth_resp);
  VERIFY_IMPL_PROTO_OP(deauth_req);
  VERIFY_IMPL_PROTO_OP(assoc_resp);
  VERIFY_IMPL_PROTO_OP(disassoc_req);
  VERIFY_IMPL_PROTO_OP(reset_req);
  VERIFY_IMPL_PROTO_OP(start_req);
  VERIFY_IMPL_PROTO_OP(stop_req);
  VERIFY_IMPL_PROTO_OP(set_keys_req);
  VERIFY_IMPL_PROTO_OP(del_keys_req);
  VERIFY_IMPL_PROTO_OP(eapol_req);

  mac_sublayer_support_t mac_sublayer_support;
  wlan_fullmac_impl_query_mac_sublayer_support(&wlan_fullmac_impl_, &mac_sublayer_support);
  if (wlan_fullmac_impl_.ops->data_queue_tx &&
      mac_sublayer_support.data_plane.data_plane_type == DATA_PLANE_TYPE_GENERIC_NETWORK_DEVICE) {
    lwarn(
        "driver implements data_queue_tx while indicating a GND data plane, data_queue_tx "
        "will not be called.");
  }

  mlme_ = std::make_unique<FullmacMlme>(this);
  ZX_DEBUG_ASSERT(mlme_ != nullptr);
  mlme_->Init();

  ZX_DEBUG_ASSERT(device_ == nullptr);
  auto status = AddDevice();
  if (status != ZX_OK) {
    lerror("could not add ethernet_impl device: %s\n", zx_status_get_string(status));
  }

  return status;
}
#undef VERIFY_IMPL_PROTO_OP
#undef VERIFY_IMPL_IFC_PROTO_OP

void Device::Unbind() {
  ltrace_fn();
  mlme_->StopMainLoop();
  device_unbind_reply(device_);
}

void Device::Release() {
  ltrace_fn();
  delete this;
}

zx_status_t Device::Start(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
                          zx::channel* out_sme_channel) {
  // We manually populate the protocol ops here so that we can verify at compile time that our rust
  // bindings have the expected parameters.
  wlan_fullmac_impl_ifc_ops_.reset(new wlan_fullmac_impl_ifc_protocol_ops_t{
      .on_scan_result = ifc->ops->on_scan_result,
      .on_scan_end = ifc->ops->on_scan_end,
      .connect_conf = ifc->ops->connect_conf,
      .auth_ind = ifc->ops->auth_ind,
      .deauth_conf = ifc->ops->deauth_conf,
      .deauth_ind = ifc->ops->deauth_ind,
      .assoc_ind = ifc->ops->assoc_ind,
      .disassoc_conf = ifc->ops->disassoc_conf,
      .disassoc_ind = ifc->ops->disassoc_ind,
      .start_conf = ifc->ops->start_conf,
      .stop_conf = ifc->ops->stop_conf,
      .eapol_conf = ifc->ops->eapol_conf,
      .on_channel_switch = ifc->ops->on_channel_switch,
      .signal_report = ifc->ops->signal_report,
      .eapol_ind = ifc->ops->eapol_ind,
      .on_pmk_available = ifc->ops->on_pmk_available,
      .sae_handshake_ind = ifc->ops->sae_handshake_ind,
      .sae_frame_rx = ifc->ops->sae_frame_rx,
      .on_wmm_status_resp = ifc->ops->on_wmm_status_resp,
  });
  return wlan_fullmac_impl_start(&wlan_fullmac_impl_, ifc->ctx, wlan_fullmac_impl_ifc_ops_.get(),
                                 out_sme_channel->reset_and_get_address());
}

void Device::StartScan(const wlan_fullmac_scan_req_t* req) {
  wlan_fullmac_impl_start_scan(&wlan_fullmac_impl_, req);
}

void Device::ConnectReq(const wlan_fullmac_connect_req_t* req) {
  eth_device_.SetEthernetStatus(&wlan_fullmac_impl_, false);
  wlan_fullmac_impl_connect_req(&wlan_fullmac_impl_, req);
}

void Device::ReconnectReq(const wlan_fullmac_reconnect_req_t* req) {
  wlan_fullmac_impl_reconnect_req(&wlan_fullmac_impl_, req);
}

void Device::AuthenticateResp(const wlan_fullmac_auth_resp_t* resp) {
  wlan_fullmac_impl_auth_resp(&wlan_fullmac_impl_, resp);
}

void Device::DeauthenticateReq(const wlan_fullmac_deauth_req_t* req) {
  eth_device_.SetEthernetStatus(&wlan_fullmac_impl_, false);
  wlan_fullmac_impl_deauth_req(&wlan_fullmac_impl_, req);
}

void Device::AssociateResp(const wlan_fullmac_assoc_resp_t* resp) {
  wlan_fullmac_impl_assoc_resp(&wlan_fullmac_impl_, resp);
}

void Device::DisassociateReq(const wlan_fullmac_disassoc_req_t* req) {
  eth_device_.SetEthernetStatus(&wlan_fullmac_impl_, false);
  wlan_fullmac_impl_disassoc_req(&wlan_fullmac_impl_, req);
}

void Device::ResetReq(const wlan_fullmac_reset_req_t* req) {
  eth_device_.SetEthernetStatus(&wlan_fullmac_impl_, false);
  wlan_fullmac_impl_reset_req(&wlan_fullmac_impl_, req);
}

void Device::StartReq(const wlan_fullmac_start_req_t* req) {
  wlan_fullmac_impl_start_req(&wlan_fullmac_impl_, req);
}

void Device::StopReq(const wlan_fullmac_stop_req_t* req) {
  wlan_fullmac_impl_stop_req(&wlan_fullmac_impl_, req);
}

void Device::SetKeysReq(const wlan_fullmac_set_keys_req_t* req,
                        wlan_fullmac_set_keys_resp_t* out_resp) {
  wlan_fullmac_impl_set_keys_req(&wlan_fullmac_impl_, req, out_resp);
}

void Device::DeleteKeysReq(const wlan_fullmac_del_keys_req_t* req) {
  wlan_fullmac_impl_del_keys_req(&wlan_fullmac_impl_, req);
}

void Device::EapolReq(const wlan_fullmac_eapol_req_t* req) {
  wlan_fullmac_impl_eapol_req(&wlan_fullmac_impl_, req);
}

void Device::QueryDeviceInfo(wlan_fullmac_query_info_t* out_resp) {
  wlan_fullmac_impl_query(&wlan_fullmac_impl_, out_resp);
}

void Device::QueryMacSublayerSupport(mac_sublayer_support_t* out_resp) {
  wlan_fullmac_impl_query_mac_sublayer_support(&wlan_fullmac_impl_, out_resp);
}

void Device::QuerySecuritySupport(security_support_t* out_resp) {
  wlan_fullmac_impl_query_security_support(&wlan_fullmac_impl_, out_resp);
}

void Device::QuerySpectrumManagementSupport(spectrum_management_support_t* out_resp) {
  wlan_fullmac_impl_query_spectrum_management_support(&wlan_fullmac_impl_, out_resp);
}

int32_t Device::GetIfaceCounterStats(wlan_fullmac_iface_counter_stats_t* out_stats) {
  return wlan_fullmac_impl_get_iface_counter_stats(&wlan_fullmac_impl_, out_stats);
}

int32_t Device::GetIfaceHistogramStats(wlan_fullmac_iface_histogram_stats_t* out_stats) {
  std::lock_guard<std::mutex> lock(get_iface_histogram_stats_lock_);
  return wlan_fullmac_impl_get_iface_histogram_stats(&wlan_fullmac_impl_, out_stats);
}

void Device::OnLinkStateChanged(bool online) {
  eth_device_.SetEthernetStatus(&wlan_fullmac_impl_, online);
}

void Device::SaeHandshakeResp(const wlan_fullmac_sae_handshake_resp_t* resp) {
  wlan_fullmac_impl_sae_handshake_resp(&wlan_fullmac_impl_, resp);
}

void Device::SaeFrameTx(const wlan_fullmac_sae_frame_t* frame) {
  wlan_fullmac_impl_sae_frame_tx(&wlan_fullmac_impl_, frame);
}

void Device::WmmStatusReq() { wlan_fullmac_impl_wmm_status_req(&wlan_fullmac_impl_); }

zx_status_t Device::EthStart(const ethernet_ifc_protocol_t* ifc) {
  return eth_device_.EthStart(ifc);
}

void Device::EthStop() { return eth_device_.EthStop(); }

zx_status_t Device::EthQuery(uint32_t options, ethernet_info_t* info) {
  std::lock_guard<std::mutex> lock(lock_);

  std::memset(info, 0, sizeof(*info));

  // features
  wlan_fullmac_query_info query_info;
  wlan_fullmac_impl_query(&wlan_fullmac_impl_, &query_info);
  info->features = ETHERNET_FEATURE_WLAN;
  if (query_info.features & WLAN_FULLMAC_FEATURE_DMA) {
    info->features |= ETHERNET_FEATURE_DMA;
  }
  if (query_info.features & WLAN_FULLMAC_FEATURE_SYNTH) {
    info->features |= ETHERNET_FEATURE_SYNTH;
  }
  if (query_info.role == WLAN_MAC_ROLE_AP) {
    info->features |= ETHERNET_FEATURE_WLAN_AP;
  }

  // mtu
  info->mtu = 1500;
  info->netbuf_size = sizeof(ethernet_netbuf_t);

  // sta
  std::memcpy(info->mac, query_info.sta_addr, ETH_ALEN);

  return ZX_OK;
}

void Device::EthQueueTx(uint32_t options, ethernet_netbuf_t* netbuf,
                        ethernet_impl_queue_tx_callback completion_cb, void* cookie) {
  eth_device_.EthQueueTx(&wlan_fullmac_impl_, options, netbuf, completion_cb, cookie);
}

zx_status_t Device::EthSetParam(uint32_t param, int32_t value, const void* data, size_t data_size) {
  return eth_device_.EthSetParam(&wlan_fullmac_impl_, param, value, data, data_size);
}

void Device::EthRecv(const uint8_t* data, size_t length, uint32_t flags) {
  eth_device_.EthRecv(data, length, flags);
}

EthDevice::EthDevice() { ltrace_fn(); }

EthDevice::~EthDevice() { ltrace_fn(); }

zx_status_t EthDevice::EthStart(const ethernet_ifc_protocol_t* ifc) {
  std::lock_guard<std::mutex> lock(lock_);
  ethernet_ifc_ = *ifc;
  eth_started_ = true;
  if (eth_online_) {
    ethernet_ifc_status(&ethernet_ifc_, ETHERNET_STATUS_ONLINE);
  }
  // TODO(fxbug.dev/51009): Inform SME that ethernet has started.
  return ZX_OK;
}

void EthDevice::EthStop() {
  std::lock_guard<std::mutex> lock(lock_);
  eth_started_ = false;
  ethernet_ifc_ = {};
}

void EthDevice::EthQueueTx(wlan_fullmac_impl_protocol_t* wlan_fullmac_impl_proto, uint32_t options,
                           ethernet_netbuf_t* netbuf, ethernet_impl_queue_tx_callback completion_cb,
                           void* cookie) {
  if (wlan_fullmac_impl_proto->ops->data_queue_tx != nullptr) {
    wlan_fullmac_impl_data_queue_tx(wlan_fullmac_impl_proto, options, netbuf, completion_cb,
                                    cookie);
  } else {
    completion_cb(cookie, ZX_ERR_NOT_SUPPORTED, netbuf);
  }
}

zx_status_t EthDevice::EthSetParam(wlan_fullmac_impl_protocol_t* wlan_fullmac_impl_proto,
                                   uint32_t param, int32_t value, const void* data,
                                   size_t data_size) {
  zx_status_t status = ZX_ERR_NOT_SUPPORTED;

  switch (param) {
    case ETHERNET_SETPARAM_PROMISC:
      // See fxbug.dev/28881: In short, the bridge mode doesn't require WLAN promiscuous mode
      // enabled.
      //               So we give a warning and return OK here to continue the bridging.
      // TODO(fxbug.dev/29113): To implement the real promiscuous mode.
      if (value == 1) {  // Only warn when enabling.
        lwarn("WLAN promiscuous not supported yet. see fxbug.dev/29113\n");
      }
      status = ZX_OK;
      break;
    case ETHERNET_SETPARAM_MULTICAST_PROMISC:
      if (wlan_fullmac_impl_proto->ops->set_multicast_promisc != nullptr) {
        return wlan_fullmac_impl_set_multicast_promisc(wlan_fullmac_impl_proto, !!value);
      } else {
        return ZX_ERR_NOT_SUPPORTED;
      }
      break;
  }

  return status;
}

void EthDevice::SetEthernetStatus(wlan_fullmac_impl_protocol_t* wlan_fullmac_impl_proto,
                                  bool online) {
  std::lock_guard<std::mutex> lock(lock_);

  // TODO(fxbug.dev/51009): Let SME handle these changes.
  if (online != eth_online_) {
    eth_online_ = online;
    if (eth_started_) {
      ethernet_ifc_status(&ethernet_ifc_, online ? ETHERNET_STATUS_ONLINE : 0);
    }
    if (wlan_fullmac_impl_proto->ops->on_link_state_changed) {
      wlan_fullmac_impl_on_link_state_changed(wlan_fullmac_impl_proto, online);
    }
  }
}

bool EthDevice::IsEthernetOnline() {
  std::lock_guard<std::mutex> lock(lock_);
  return eth_online_;
}

void EthDevice::EthRecv(const uint8_t* data, size_t length, uint32_t flags) {
  std::lock_guard<std::mutex> lock(lock_);
  if (eth_started_) {
    ethernet_ifc_recv(&ethernet_ifc_, data, length, flags);
  }
}

}  // namespace wlanif
