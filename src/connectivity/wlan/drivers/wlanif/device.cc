// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device.h"

#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/stats/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <net/ethernet.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>

#include <wlan/common/ieee80211_codes.h>

#include "convert.h"
#include "debug.h"
#include "fidl/fuchsia.wlan.fullmac/cpp/wire_types.h"
#include "fidl/fuchsia.wlan.ieee80211/cpp/common_types.h"
#include "fuchsia/wlan/common/c/banjo.h"
#include "zircon/system/public/zircon/assert.h"

namespace wlanif {

Device::Device(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("wlanif", std::move(start_args), std::move(driver_dispatcher)),
      rust_mlme_(nullptr, delete_fullmac_mlme),
      parent_node_(fidl::WireClient(std::move(node()), dispatcher())) {
  zx::result logger = fdf::Logger::Create(*incoming(), dispatcher(), "wlanif", FUCHSIA_LOG_INFO);
  ZX_ASSERT_MSG(logger.is_ok(), "%s", logger.status_string());
  logger_ = std::move(logger.value());
  ltrace_fn(*logger_);
}

Device::~Device() { ltrace_fn(*logger_); }

zx::result<> Device::Start() {
  {
    // Create a dispatcher to wait on the runtime channel
    auto dispatcher =
        fdf::SynchronizedDispatcher::Create(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
                                            "wlan_fullmac_ifc_server", [&](fdf_dispatcher_t*) {});

    if (dispatcher.is_error()) {
      FDF_LOGL(ERROR, *logger_, "Creating server dispatcher error : %s",
               zx_status_get_string(dispatcher.status_value()));
      return zx::error(dispatcher.status_value());
    }
    server_dispatcher_ = *std::move(dispatcher);
  }

  zx::result<fidl::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl>> client_end =
      incoming()->Connect<fuchsia_wlan_fullmac::Service::WlanFullmacImpl>();
  if (client_end.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Connect to FullmacImpl failed: %s", client_end.status_string());
    return client_end.take_error();
  }

  auto rust_device = rust_fullmac_device_ffi_t{
      .device = static_cast<void*>(this),
      .start_fullmac_ifc_server = [](void* device, const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
                                     zx_handle_t ifc_server_end_handle) -> zx_status_t {
        return static_cast<Device*>(device)->StartFullmacIfcServer(ifc, ifc_server_end_handle);
      },
  };

  zx_handle_t client_end_handle = client_end.value().TakeHandle().release();
  std::lock_guard lock(rust_mlme_lock_);
  rust_mlme_ =
      RustFullmacMlme(start_fullmac_mlme(rust_device, client_end_handle), delete_fullmac_mlme);
  if (!rust_mlme_) {
    FDF_LOGL(ERROR, *logger_, "Rust MLME is not valid");
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  return zx::ok();
}

void Device::PrepareStop(fdf::PrepareStopCompleter completer) {
  // Destroy the WlanFullmacImplIfc FFI bridge.
  {
    std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
    wlan_fullmac_impl_ifc_banjo_protocol_.reset();
  }
  {
    std::lock_guard lock(rust_mlme_lock_);
    if (rust_mlme_) {
      stop_fullmac_mlme(rust_mlme_.get());
    }
  }
  completer(zx::ok());
}

zx_status_t Device::StartFullmacIfcServer(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
                                          zx_handle_t ifc_server_end_handle) {
  auto server_end =
      fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(zx::channel(ifc_server_end_handle));

  if (!server_end.is_valid()) {
    FDF_LOGL(ERROR, *logger_, "MLME provided invalid handle");
    return ZX_ERR_BAD_HANDLE;
  }

  // We manually populate the protocol ops here so that we can verify at compile time
  // that our rust bindings have the expected parameters.
  wlan_fullmac_impl_ifc_banjo_protocol_ops_.reset(new wlan_fullmac_impl_ifc_banjo_protocol_ops_t{
      .on_scan_result = ifc->ops->on_scan_result,
      .on_scan_end = ifc->ops->on_scan_end,
      .connect_conf = ifc->ops->connect_conf,
      .roam_start_ind = ifc->ops->roam_start_ind,
      .roam_result_ind = ifc->ops->roam_result_ind,
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

  {
    std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
    wlan_fullmac_impl_ifc_banjo_protocol_ =
        std::make_unique<wlan_fullmac_impl_ifc_banjo_protocol_t>();
    wlan_fullmac_impl_ifc_banjo_protocol_->ops = wlan_fullmac_impl_ifc_banjo_protocol_ops_.get();
    wlan_fullmac_impl_ifc_banjo_protocol_->ctx = ifc->ctx;
  }

  fidl::BindServer(server_dispatcher_.async_dispatcher(), std::move(server_end), this,
                   std::mem_fn(&Device::OnUnbound));

  return ZX_OK;
}

void Device::OnUnbound(fidl::UnbindInfo info,
                       fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc>) {
  if (!info.is_user_initiated()) {
    FDF_LOGL(INFO, *logger_, "Wlanif shutting down: %s", info.FormatDescription().c_str());
  }
}

void Device::handle_unknown_event(
    fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) {
  FDF_LOGL(ERROR, *logger_, "Received unknown NodeController FIDL event with ordinal %lu",
           metadata.event_ordinal);
}

// Implementation of fuchsia_wlan_fullmac::WlanFullmacImplIfc.
void Device::OnScanResult(OnScanResultRequestView request, OnScanResultCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_scan_result_t scan_result;
    ConvertFullmacScanResult(request->result, &scan_result);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_scan_result(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &scan_result);
  }
  completer.Reply();
}

void Device::OnScanEnd(OnScanEndRequestView request, OnScanEndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_scan_end_t scan_end;
    ConvertScanEnd(request->end, &scan_end);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_scan_end(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &scan_end);
  }
  completer.Reply();
}

void Device::ConnectConf(ConnectConfRequestView request, ConnectConfCompleter::Sync& completer) {
  wlan_fullmac_connect_confirm_t connect_conf;
  ConvertConnectConfirm(request->resp, &connect_conf);
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->connect_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &connect_conf);
  }
  completer.Reply();
}

void Device::RoamStartInd(RoamStartIndRequestView request, RoamStartIndCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/357134611) Reduce or eliminate these fatal assertions.
  ZX_ASSERT(request->has_selected_bssid());
  ZX_ASSERT(request->selected_bssid().size() == ETH_ALEN);

  // It is possible that a roam attempt can fail because a Fullmac driver cannot obtain
  // the BSS description, or the BSS description is malformed.
  // In spite of this, we still want Fullmac to notify SME that a roam
  // attempt started, and this is why we don't assert on missing BSS description here.
  bss_description_t selected_bss;
  memset(&selected_bss, 0, sizeof(bss_description_t));
  if (request->has_selected_bss()) {
    ConvertBssDescription(request->selected_bss(), &selected_bss);
  }
  const auto original_association_maintained = request->has_original_association_maintained()
                                                   ? request->original_association_maintained()
                                                   : false;

  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->roam_start_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, request->selected_bssid().data(), &selected_bss,
        original_association_maintained);
  }
  completer.Reply();
}

void Device::RoamResultInd(RoamResultIndRequestView request,
                           RoamResultIndCompleter::Sync& completer) {
  ZX_ASSERT(request->has_selected_bssid());
  ZX_ASSERT(request->selected_bssid().size() == ETH_ALEN);
  const auto status_code = request->has_status_code()
                               ? request->status_code()
                               : fuchsia_wlan_ieee80211::StatusCode::kRefusedReasonUnspecified;

  const auto original_association_maintained = request->has_original_association_maintained()
                                                   ? request->original_association_maintained()
                                                   : false;
  const auto target_bss_authenticated =
      request->has_target_bss_authenticated() ? request->target_bss_authenticated() : false;
  const auto association_id = request->has_association_id() ? request->association_id() : 0;
  const auto association_ies =
      request->has_association_ies() ? request->association_ies().data() : nullptr;
  const size_t association_ies_count =
      request->has_association_ies() ? request->association_ies().count() : 0;

  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->roam_result_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, request->selected_bssid().data(),
        static_cast<status_code_t>(status_code), original_association_maintained,
        target_bss_authenticated, association_id, association_ies, association_ies_count);
  }
  completer.Reply();
}

void Device::AuthInd(AuthIndRequestView request, AuthIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_auth_ind_t auth_ind;
    ConvertAuthInd(request->resp, &auth_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->auth_ind(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                        &auth_ind);
  }
  completer.Reply();
}

void Device::DeauthConf(DeauthConfRequestView request, DeauthConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    ZX_ASSERT(request->has_peer_sta_address());
    ZX_ASSERT(request->peer_sta_address().size() == ETH_ALEN);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->deauth_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, request->peer_sta_address().data());
  }
  completer.Reply();
}

void Device::DeauthInd(DeauthIndRequestView request, DeauthIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_deauth_indication_t deauth_ind;
    ConvertDeauthInd(request->ind, &deauth_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->deauth_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &deauth_ind);
  }
  completer.Reply();
}

void Device::AssocInd(AssocIndRequestView request, AssocIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_assoc_ind_t assoc_ind;
    ConvertAssocInd(request->resp, &assoc_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->assoc_ind(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                         &assoc_ind);
  }
  completer.Reply();
}

void Device::DisassocConf(DisassocConfRequestView request, DisassocConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_disassoc_confirm_t disassoc_conf;
    disassoc_conf.status = request->resp.status;
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->disassoc_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &disassoc_conf);
  }
  completer.Reply();
}

void Device::DisassocInd(DisassocIndRequestView request, DisassocIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_disassoc_indication_t disassoc_ind;
    ConvertDisassocInd(request->ind, &disassoc_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->disassoc_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &disassoc_ind);
  }
  completer.Reply();
}

void Device::StartConf(StartConfRequestView request, StartConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_start_confirm_t start_conf;
    start_conf.result_code = ConvertStartResultCode(request->resp.result_code);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->start_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &start_conf);
  }
  completer.Reply();
}

void Device::StopConf(StopConfRequestView request, StopConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_stop_confirm_t stop_conf;
    stop_conf.result_code = ConvertStopResultCode(request->resp.result_code);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->stop_conf(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                         &stop_conf);
  }
  completer.Reply();
}

void Device::EapolConf(EapolConfRequestView request, EapolConfCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_eapol_confirm_t eapol_conf;
    ConvertEapolConf(request->resp, &eapol_conf);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->eapol_conf(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &eapol_conf);
  }
  completer.Reply();
}

void Device::OnChannelSwitch(OnChannelSwitchRequestView request,
                             OnChannelSwitchCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_channel_switch_info channel_switch_info;
    channel_switch_info.new_channel = request->ind.new_channel;
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_channel_switch(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &channel_switch_info);
  }
  completer.Reply();
}

void Device::SignalReport(SignalReportRequestView request, SignalReportCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_signal_report_indication_t signal_report_ind;
    signal_report_ind.rssi_dbm = request->ind.rssi_dbm;
    signal_report_ind.snr_db = request->ind.snr_db;
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->signal_report(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &signal_report_ind);
  }
  completer.Reply();
}

void Device::EapolInd(EapolIndRequestView request, EapolIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_eapol_indication_t eapol_ind;
    ConvertEapolIndication(request->ind, &eapol_ind);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->eapol_ind(wlan_fullmac_impl_ifc_banjo_protocol_->ctx,
                                                         &eapol_ind);
  }
  completer.Reply();
}

void Device::OnPmkAvailable(OnPmkAvailableRequestView request,
                            OnPmkAvailableCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_pmk_info_t pmk_info;
    pmk_info.pmk_list = request->info.pmk.data();
    pmk_info.pmk_count = request->info.pmk.count();
    pmk_info.pmkid_list = request->info.pmkid.data();
    pmk_info.pmkid_count = request->info.pmkid.count();
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_pmk_available(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &pmk_info);
  }
  completer.Reply();
}

void Device::SaeHandshakeInd(SaeHandshakeIndRequestView request,
                             SaeHandshakeIndCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_sae_handshake_ind_t sae_handshake_ind;
    memcpy(sae_handshake_ind.peer_sta_address, request->ind.peer_sta_address.data(), ETH_ALEN);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->sae_handshake_ind(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &sae_handshake_ind);
  }
  completer.Reply();
}

void Device::SaeFrameRx(SaeFrameRxRequestView request, SaeFrameRxCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_fullmac_sae_frame_t sae_frame;
    ConvertSaeFrame(request->frame, &sae_frame);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->sae_frame_rx(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, &sae_frame);
  }
  completer.Reply();
}

void Device::OnWmmStatusResp(OnWmmStatusRespRequestView request,
                             OnWmmStatusRespCompleter::Sync& completer) {
  std::lock_guard guard(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  if (wlan_fullmac_impl_ifc_banjo_protocol_ != nullptr) {
    wlan_wmm_parameters_t wmm_params;
    ConvertWmmParams(request->wmm_params, &wmm_params);
    wlan_fullmac_impl_ifc_banjo_protocol_ops_->on_wmm_status_resp(
        wlan_fullmac_impl_ifc_banjo_protocol_->ctx, request->status, &wmm_params);
  }
  completer.Reply();
}

}  // namespace wlanif
FUCHSIA_DRIVER_EXPORT(::wlanif::Device);
