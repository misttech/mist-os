// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlantap-mac.h"

#include <wlan/common/channel.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl_driver/cpp/wire_messaging_declarations.h"
#include "utils.h"

namespace {
// Large enough to back a full WlanSoftmacQueryResponse FIDL struct.
constexpr size_t kWlanSoftmacQueryResponseBufferSize = 5120;
}  // namespace

namespace wlan {

WlantapMac::WlantapMac(Listener* listener, wlan_common::WlanMacRole role,
                       std::shared_ptr<wlan_tap::WlantapPhyConfig> config, zx::channel sme_channel)
    : listener_(listener),
      role_(role),
      phy_config_(std::move(config)),
      sme_channel_(std::move(sme_channel)) {}

void WlantapMac::Query(fdf::Arena& arena, QueryCompleter::Sync& completer) {
  FDF_LOG(INFO, "Query(): %u", phy_config_->hardware_capability);
  fidl::Arena<kWlanSoftmacQueryResponseBufferSize> table_arena;
  wlan_softmac::WlanSoftmacQueryResponse resp;
  ConvertTapPhyConfig(&resp, *phy_config_, table_arena);
  completer.buffer(arena).ReplySuccess(resp);
}

fidl::ProtocolHandler<fuchsia_wlan_softmac::WlanSoftmac> WlantapMac::ProtocolHandler() {
  return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                 fidl::kIgnoreBindingClosure);
}

void WlantapMac::QueryDiscoverySupport(fdf::Arena& arena,
                                       QueryDiscoverySupportCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(phy_config_->discovery_support);
}

void WlantapMac::QueryMacSublayerSupport(fdf::Arena& arena,
                                         QueryMacSublayerSupportCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(phy_config_->mac_sublayer_support);
}

void WlantapMac::QuerySecuritySupport(fdf::Arena& arena,
                                      QuerySecuritySupportCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(phy_config_->security_support);
}

void WlantapMac::QuerySpectrumManagementSupport(
    fdf::Arena& arena, QuerySpectrumManagementSupportCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(phy_config_->spectrum_management_support);
}

void WlantapMac::Start(StartRequestView request, fdf::Arena& arena,
                       StartCompleter::Sync& completer) {
  FDF_LOG(INFO, "Calling Start()");
  if (!sme_channel_.is_valid()) {
    completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_BOUND);
    return;
  }

  listener_->WlantapMacStart(std::move(request->ifc));
  completer.buffer(arena).ReplySuccess(std::move(sme_channel_));
}

void WlantapMac::Stop(fdf::Arena& arena, StopCompleter::Sync& completer) {
  listener_->WlantapMacStop();
  completer.buffer(arena).Reply();
}

void WlantapMac::QueueTx(QueueTxRequestView request, fdf::Arena& arena,
                         QueueTxCompleter::Sync& completer) {
  listener_->WlantapMacQueueTx(request->packet);
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::SetChannel(SetChannelRequestView request, fdf::Arena& arena,
                            SetChannelCompleter::Sync& completer) {
  if (!wlan::common::IsValidChan(request->channel())) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  listener_->WlantapMacSetChannel(request->channel());
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::JoinBss(JoinBssRequestView request, fdf::Arena& arena,
                         JoinBssCompleter::Sync& completer) {
  bool expected_remote = role_ == wlan_common::WlanMacRole::kClient;
  if (request->join_request.remote() != expected_remote) {
    completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  listener_->WlantapMacJoinBss(request->join_request);
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::EnableBeaconing(EnableBeaconingRequestView request, fdf::Arena& arena,
                                 EnableBeaconingCompleter::Sync& completer) {
  // This is the test driver, so we can just pretend beaconing was enabled.
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::DisableBeaconing(fdf::Arena& arena, DisableBeaconingCompleter::Sync& completer) {
  // This is the test driver, so we can just pretend the beacon was configured.
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::StartPassiveScan(StartPassiveScanRequestView request, fdf::Arena& arena,
                                  StartPassiveScanCompleter::Sync& completer) {
  uint64_t scan_id = 111;
  listener_->WlantapMacStartScan(scan_id);
  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_softmac::wire::WlanSoftmacStartPassiveScanResponse::Builder(fidl_arena);
  builder.scan_id(scan_id);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void WlantapMac::StartActiveScan(StartActiveScanRequestView request, fdf::Arena& arena,
                                 StartActiveScanCompleter::Sync& completer) {
  uint64_t scan_id = 222;
  listener_->WlantapMacStartScan(scan_id);

  fidl::Arena fidl_arena;
  auto builder =
      fuchsia_wlan_softmac::wire::WlanSoftmacStartActiveScanResponse::Builder(fidl_arena);
  builder.scan_id(scan_id);
  completer.buffer(arena).ReplySuccess(builder.Build());
}

void WlantapMac::InstallKey(InstallKeyRequestView request, fdf::Arena& arena,
                            InstallKeyCompleter::Sync& completer) {
  listener_->WlantapMacSetKey(*request);
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::NotifyAssociationComplete(NotifyAssociationCompleteRequestView request,
                                           fdf::Arena& arena,
                                           NotifyAssociationCompleteCompleter::Sync& completer) {
  // This is the test driver, so we can just pretend the association was configured.
  // TODO(fxbug.dev/28907): Evaluate the use and implement
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::ClearAssociation(ClearAssociationRequestView request, fdf::Arena& arena,
                                  ClearAssociationCompleter::Sync& completer) {
  // TODO(fxbug.dev/28907): Evaluate the use and implement. Association is never
  // configured, so there is nothing to clear.
  completer.buffer(arena).ReplySuccess();
}

void WlantapMac::CancelScan(CancelScanRequestView request, fdf::Arena& arena,
                            CancelScanCompleter::Sync& completer) {
  ZX_PANIC("CancelScan is not supported.");
}

void WlantapMac::UpdateWmmParameters(UpdateWmmParametersRequestView request, fdf::Arena& arena,
                                     UpdateWmmParametersCompleter::Sync& completer) {
  ZX_PANIC("UpdateWmmParameters is not supported.");
}

}  // namespace wlan
