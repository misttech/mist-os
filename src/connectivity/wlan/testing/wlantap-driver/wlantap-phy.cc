// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlantap-phy.h"

#include <fidl/fuchsia.wlan.common/cpp/fidl.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl_driver/cpp/wire_messaging_declarations.h"
#include "utils.h"

namespace wlan {
namespace wlan_common = fuchsia_wlan_common::wire;

namespace {

wlan_tap::SetKeyArgs ToSetKeyArgs(const wlan_softmac::WlanKeyConfiguration& config) {
  ZX_ASSERT(config.has_protection() && config.has_cipher_oui() && config.has_cipher_type() &&
            config.has_key_type() && config.has_peer_addr() && config.has_key_idx() &&
            config.has_key());

  auto set_key_args = wlan_tap::SetKeyArgs{
      .config =
          wlan_tap::WlanKeyConfig{
              .protection = static_cast<uint8_t>(config.protection()),
              .cipher_oui = config.cipher_oui(),
              .cipher_type = config.cipher_type(),
              .key_type = static_cast<uint8_t>(config.key_type()),
              .peer_addr = config.peer_addr(),
              .key_idx = config.key_idx(),
          },
  };
  set_key_args.config.key = fidl::VectorView<uint8_t>::FromExternal(
      const_cast<uint8_t*>(config.key().begin()), config.key().count());
  return set_key_args;
}

wlan_tap::TxArgs ToTxArgs(const wlan_softmac::WlanTxPacket pkt) {
  if (pkt.info.phy < wlan_common::WlanPhyType::kDsss ||
      pkt.info.phy > wlan_common::WlanPhyType::kHe) {
    ZX_PANIC("Unknown PHY in wlan_tx_packet_t: %u.", static_cast<uint32_t>(pkt.info.phy));
  }

  auto cbw = static_cast<uint32_t>(pkt.info.channel_bandwidth);
  wlan_tap::WlanTxInfo tap_info = {
      .tx_flags = pkt.info.tx_flags,
      .valid_fields = pkt.info.valid_fields,
      .tx_vector_idx = pkt.info.tx_vector_idx,
      .phy = pkt.info.phy,
      .cbw = static_cast<uint8_t>(cbw),
      .mcs = pkt.info.mcs,
  };
  auto tx_args = wlan_tap::TxArgs{
      .packet = wlan_tap::WlanTxPacket{.data = pkt.mac_frame, .info = tap_info},
  };

  return tx_args;
}

}  // namespace

WlantapPhy::WlantapPhy(zx::channel user_channel,
                       std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config,
                       fidl::ClientEnd<fuchsia_driver_framework::NodeController> phy_controller)
    : phy_config_(std::move(phy_config)),
      name_("wlantap-phy:" + std::string(phy_config_->name.get())),
      user_binding_(fidl::BindServer(
          fdf::Dispatcher::GetCurrent()->async_dispatcher(),
          fidl::ServerEnd<fuchsia_wlan_tap::WlantapPhy>(std::move(user_channel)), this)),
      phy_controller_(std::move(phy_controller)) {}

zx_status_t WlantapPhy::SetCountry(wlan_tap::SetCountryArgs args) {
  fidl::Status status = fidl::WireSendEvent(user_binding_)->SetCountry(args);
  if (!status.ok()) {
    FDF_LOG(ERROR, "%s: SetCountry() failed: user_binding not bound", status.status_string());

    return status.status();
  }
  return ZX_OK;
}

// wlan_tap::WlantapPhy impl

void WlantapPhy::Shutdown(ShutdownCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: Shutdown", name_.c_str());
  FDF_LOG(INFO, "%s: PHY device shutdown initiated.", name_.c_str());
  // user_binding_.Unbind();
  ZX_ASSERT(phy_controller_.is_valid());

  auto status = phy_controller_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove phy: %s", status.status_string());
  }
  completer.Reply();
}

void WlantapPhy::Rx(RxRequestView request, RxCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: Rx(%zu bytes)", name_.c_str(), request->data.count());
  if (!wlan_softmac_ifc_client_.is_valid()) {
    FDF_LOG(ERROR, "No WlantapMac present.");
    return;
  }
  auto rx_flags = wlan_softmac::WlanRxInfoFlags::TruncatingUnknown(request->info.rx_flags);
  auto valid_fields = wlan_softmac::WlanRxInfoValid::TruncatingUnknown(request->info.valid_fields);
  wlan_softmac::WlanRxInfo converted_info = {.rx_flags = rx_flags,
                                             .valid_fields = valid_fields,
                                             .phy = request->info.phy,
                                             .data_rate = request->info.data_rate,
                                             .channel = request->info.channel,
                                             .mcs = request->info.mcs,
                                             .rssi_dbm = request->info.rssi_dbm,
                                             .snr_dbh = request->info.snr_dbh};

  wlan_softmac::WlanRxPacket rx_packet = {.mac_frame = request->data, .info = converted_info};
  auto arena = fdf::Arena::Create(0, 0);
  wlan_softmac_ifc_client_.buffer(*arena)->Recv(rx_packet).ThenExactlyOnce(
      [completer = completer.ToAsync()](
          fdf::WireUnownedResult<fuchsia_wlan_softmac::WlanSoftmacIfc::Recv>& result) {
        FDF_LOG(INFO, "Recv completed");
      });
  FDF_LOG(DEBUG, "%s: Rx done", name_.c_str());
}

void WlantapPhy::ReportTxResult(ReportTxResultRequestView request,
                                ReportTxResultCompleter::Sync& completer) {
  if (!phy_config_->quiet || report_tx_status_count_ < 32) {
    FDF_LOG(INFO, "%s: ReportTxResult %zu", name_.c_str(), report_tx_status_count_);
  }

  if (!wlan_softmac_ifc_client_.is_valid()) {
    FDF_LOG(ERROR, "WlantapMacStart() not called.");
    return;
  }

  ++report_tx_status_count_;

  auto arena = fdf::Arena::Create(0, 0);
  wlan_softmac_ifc_client_.buffer(*arena)
      ->ReportTxResult(request->txr)
      .ThenExactlyOnce(
          [this](fdf::WireUnownedResult<fuchsia_wlan_softmac::WlanSoftmacIfc::ReportTxResult>&
                     result) {
            if (!result.ok()) {
              FDF_LOG(ERROR, "Failed to report tx status up. Status: %s",
                      zx_status_get_string(result.status()));
              return;
            }

            FDF_LOG(DEBUG, "%s: ScanComplete done", name_.c_str());
            if (!phy_config_->quiet || report_tx_status_count_ <= 32) {
              FDF_LOG(DEBUG, "%s: ReportTxResult %zu done", name_.c_str(), report_tx_status_count_);
            }
          });
}

void WlantapPhy::ScanComplete(ScanCompleteRequestView request,
                              ScanCompleteCompleter::Sync& completer) {
  FDF_LOG(INFO, "%s: ScanComplete(%u)", name_.c_str(), request->status);
  if (!wlan_softmac_ifc_client_.is_valid()) {
    FDF_LOG(ERROR, "WlantapMacStart() not called.");
    return;
  }

  auto arena = fdf::Arena::Create(0, 0);
  fidl::Arena fidl_arena;

  using Request = fuchsia_wlan_softmac::wire::WlanSoftmacIfcNotifyScanCompleteRequest;
  auto scan_complete_req =
      Request::Builder(fidl_arena).scan_id(request->scan_id).status(request->status).Build();

  wlan_softmac_ifc_client_.buffer(*arena)
      ->NotifyScanComplete(scan_complete_req)
      .ThenExactlyOnce(
          [](fdf::WireUnownedResult<fuchsia_wlan_softmac::WlanSoftmacIfc::NotifyScanComplete>&
                 result) {
            if (!result.ok()) {
              FDF_LOG(ERROR, "Failed to send scan complete notification up. Status: %s",
                      zx_status_get_string(result.status()));
            } else {
              FDF_LOG(INFO, "ScanComplete done");
            }
          });
}

// WlantapMac::Listener impl

void WlantapPhy::WlantapMacStart(fdf::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfc> ifc_client) {
  FDF_LOG(INFO, "%s: WlantapMacStart", name_.c_str());
  wlan_softmac_ifc_client_ = fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmacIfc>(
      std::move(ifc_client), fdf::Dispatcher::GetCurrent()->get());

  fidl::Status status = fidl::WireSendEvent(user_binding_)->WlanSoftmacStart();
  if (!status.ok()) {
    FDF_LOG(ERROR, "%s: WlanSoftmacStart() failed", status.status_string());
    return;
  }

  FDF_LOG(INFO, "%s: WlantapMacStart done", name_.c_str());
}

void WlantapPhy::WlantapMacStop() { FDF_LOG(INFO, "%s: WlantapMacStop", name_.c_str()); }

void WlantapPhy::WlantapMacQueueTx(const fuchsia_wlan_softmac::wire::WlanTxPacket& pkt) {
  size_t pkt_size = pkt.mac_frame.count();
  if (!phy_config_->quiet || report_tx_status_count_ < 32) {
    FDF_LOG(INFO, "%s: WlantapMacQueueTx, size=%zu, tx_report_count=%zu", name_.c_str(), pkt_size,
            report_tx_status_count_);
  }

  fidl::Status status = fidl::WireSendEvent(user_binding_)->Tx(ToTxArgs(pkt));
  if (!status.ok()) {
    FDF_LOG(ERROR, "%s: Tx() failed", status.status_string());
    return;
  }
  if (!phy_config_->quiet || report_tx_status_count_ < 32) {
    FDF_LOG(DEBUG, "%s: WlantapMacQueueTx done(%zu bytes), tx_report_count=%zu", name_.c_str(),
            pkt_size, report_tx_status_count_);
  }
}

void WlantapPhy::WlantapMacSetChannel(const wlan_common::WlanChannel& channel) {
  if (!phy_config_->quiet) {
    FDF_LOG(INFO, "%s: WlantapMacSetChannel channel=%u", name_.c_str(), channel.primary);
  }

  fidl::Status status = fidl::WireSendEvent(user_binding_)->SetChannel({.channel = channel});
  if (!status.ok()) {
    FDF_LOG(ERROR, "%s: SetChannel() failed", status.status_string());
    return;
  }

  if (!phy_config_->quiet) {
    FDF_LOG(DEBUG, "%s: WlantapMacSetChannel done", name_.c_str());
  }
}

void WlantapPhy::WlantapMacJoinBss(const wlan_common::JoinBssRequest& config) {
  FDF_LOG(INFO, "%s: WlantapMacJoinBss", name_.c_str());

  fidl::Status status = fidl::WireSendEvent(user_binding_)->JoinBss({.config = config});
  if (!status.ok()) {
    FDF_LOG(ERROR, "%s: JoinBss() failed", status.status_string());
    return;
  }

  FDF_LOG(DEBUG, "%s: WlantapMacJoinBss done", name_.c_str());
}

void WlantapPhy::WlantapMacStartScan(const uint64_t scan_id) {
  FDF_LOG(INFO, "%s: WlantapMacStartScan", name_.c_str());

  fidl::Status status = fidl::WireSendEvent(user_binding_)
                            ->StartScan({
                                .scan_id = scan_id,
                            });
  if (!status.ok()) {
    FDF_LOG(ERROR, "%s: StartScan() failed", status.status_string());
    return;
  }
  FDF_LOG(INFO, "%s: WlantapMacStartScan done", name_.c_str());
}

void WlantapPhy::WlantapMacSetKey(const wlan_softmac::WlanKeyConfiguration& key_config) {
  FDF_LOG(INFO, "%s: WlantapMacSetKey", name_.c_str());

  fidl::Status status = fidl::WireSendEvent(user_binding_)->SetKey(ToSetKeyArgs(key_config));
  if (!status.ok()) {
    FDF_LOG(ERROR, "%s: SetKey() failed", status.status_string());
    return;
  }

  FDF_LOG(DEBUG, "%s: WlantapMacSetKey done", name_.c_str());
}

}  // namespace wlan
