// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlantap-mac.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <mutex>

#include <ddktl/device.h>
#include <wlan/common/channel.h>
#include <wlan/common/phy.h>

#include "utils.h"

namespace wlan {

namespace wlan_common = fuchsia_wlan_common::wire;
namespace wlan_softmac = fuchsia_wlan_softmac::wire;

namespace {

// TODO(fxbug.dev/93459) Prune unnecessary fields from phy_config
struct WlantapMacImpl : WlantapMac,
                        public ddk::Device<WlantapMacImpl, ddk::Initializable, ddk::Unbindable>,
                        public fdf::WireServer<fuchsia_wlan_softmac::WlanSoftmac> {
  WlantapMacImpl(zx_device_t* phy_device, wlan_common::WlanMacRole role,
                 const std::shared_ptr<const wlan_tap::WlantapPhyConfig> phy_config,
                 Listener* listener, zx::channel sme_channel)
      : ddk::Device<WlantapMacImpl, ddk::Initializable, ddk::Unbindable>(phy_device),
        role_(role),
        phy_config_(phy_config),
        listener_(listener),
        sme_channel_(std::move(sme_channel)),
        outgoing_dir_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())),
        serving_wlan_softmac_instance_(false) {}

  zx_status_t InitWlanSoftmacIfcClient() {
    // Create dispatcher for FIDL client of WlanSoftmacIfc protocol.
    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        {}, WLAN_SOFTMAC_IFC_DISPATCHER_NAME, [&](fdf_dispatcher_t*) {
          if (unbind_txn_) {
            unbind_txn_->Reply();
            return;
          }
          zxlogf(ERROR, "%s shutdown for reason other than MAC device unbind.",
                 WLAN_SOFTMAC_IFC_DISPATCHER_NAME);
        });

    if (dispatcher.is_error()) {
      return dispatcher.status_value();
    }

    wlan_softmac_ifc_dispatcher_ = *std::move(dispatcher);

    return ZX_OK;
  }

  void DdkInit(ddk::InitTxn txn) {
    auto ret = InitWlanSoftmacIfcClient();
    ZX_ASSERT_MSG(ret == ZX_OK, "%s(): %s create failed%s\n", __func__,
                  WLAN_SOFTMAC_IFC_DISPATCHER_NAME, zx_status_get_string(ret));
    txn.Reply(ZX_OK);
  }

  void DdkUnbind(ddk::UnbindTxn txn) {
    // ddk::UnbindTxn::Reply() will be called when the WlanSoftmacIfc dispatcher
    // is shutdown.
    unbind_txn_ = std::move(txn);
    zx::result res = outgoing_dir_.RemoveService<fuchsia_wlan_softmac::Service>("default");
    if (res.is_error()) {
      zxlogf(ERROR, "Failed to remove WlanSoftmac service from outgoing directory: %s",
             res.status_string());
    }
    wlan_softmac_ifc_dispatcher_.ShutdownAsync();
  }

  void DdkRelease() { delete this; }

  // WlanSoftmac protocol impl

  static constexpr size_t kWlanSoftmacQueryResponseBufferSize =
      fidl::MaxSizeInChannel<fuchsia_wlan_softmac::wire::WlanSoftmacQueryResponse,
                             fidl::MessageDirection::kSending>();

  void Query(fdf::Arena& arena, QueryCompleter::Sync& completer) override {
    fidl::Arena<kWlanSoftmacQueryResponseBufferSize> table_arena;
    wlan_softmac::WlanSoftmacQueryResponse resp;
    ConvertTapPhyConfig(&resp, *phy_config_, table_arena);
    completer.buffer(arena).ReplySuccess(resp);
  }

  void QueryDiscoverySupport(fdf::Arena& arena,
                             QueryDiscoverySupportCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess(phy_config_->discovery_support);
  }

  void QueryMacSublayerSupport(fdf::Arena& arena,
                               QueryMacSublayerSupportCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess(phy_config_->mac_sublayer_support);
  }

  void QuerySecuritySupport(fdf::Arena& arena,
                            QuerySecuritySupportCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess(phy_config_->security_support);
  }

  void QuerySpectrumManagementSupport(
      fdf::Arena& arena, QuerySpectrumManagementSupportCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess(phy_config_->spectrum_management_support);
  }

  void Start(StartRequestView request, fdf::Arena& arena,
             StartCompleter::Sync& completer) override {
    {
      std::lock_guard<std::mutex> guard(lock_);
      if (!sme_channel_.is_valid()) {
        completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_BOUND);
        return;
      }
      wlan_softmac_ifc_client_ = fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmacIfc>(
          std::move(request->ifc), wlan_softmac_ifc_dispatcher_.get());
    }
    listener_->WlantapMacStart();
    completer.buffer(arena).ReplySuccess(std::move(sme_channel_));
  }

  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer) override {
    listener_->WlantapMacStop();
    completer.buffer(arena).Reply();
  }

  void QueueTx(QueueTxRequestView request, fdf::Arena& arena,
               QueueTxCompleter::Sync& completer) override {
    listener_->WlantapMacQueueTx(request->packet);
    completer.buffer(arena).ReplySuccess();
  }

  void SetChannel(SetChannelRequestView request, fdf::Arena& arena,
                  SetChannelCompleter::Sync& completer) override {
    if (!wlan::common::IsValidChan(request->channel())) {
      completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
    listener_->WlantapMacSetChannel(request->channel());
    completer.buffer(arena).ReplySuccess();
  }

  void JoinBss(JoinBssRequestView request, fdf::Arena& arena,
               JoinBssCompleter::Sync& completer) override {
    bool expected_remote = role_ == wlan_common::WlanMacRole::kClient;
    if (request->join_request.remote() != expected_remote) {
      completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
    listener_->WlantapMacJoinBss(request->join_request);
    completer.buffer(arena).ReplySuccess();
  }

  void EnableBeaconing(EnableBeaconingRequestView request, fdf::Arena& arena,
                       EnableBeaconingCompleter::Sync& completer) override {
    // This is the test driver, so we can just pretend beaconing was enabled.
    completer.buffer(arena).ReplySuccess();
  }

  void DisableBeaconing(fdf::Arena& arena, DisableBeaconingCompleter::Sync& completer) override {
    // This is the test driver, so we can just pretend the beacon was configured.
    completer.buffer(arena).ReplySuccess();
  }

  void StartPassiveScan(StartPassiveScanRequestView request, fdf::Arena& arena,
                        StartPassiveScanCompleter::Sync& completer) override {
    uint64_t scan_id = 111;
    listener_->WlantapMacStartScan(scan_id);
    fidl::Arena fidl_arena;
    auto builder =
        fuchsia_wlan_softmac::wire::WlanSoftmacStartPassiveScanResponse::Builder(fidl_arena);
    builder.scan_id(scan_id);
    completer.buffer(arena).ReplySuccess(builder.Build());
  }

  void StartActiveScan(StartActiveScanRequestView request, fdf::Arena& arena,
                       StartActiveScanCompleter::Sync& completer) override {
    uint64_t scan_id = 222;
    listener_->WlantapMacStartScan(scan_id);

    fidl::Arena fidl_arena;
    auto builder =
        fuchsia_wlan_softmac::wire::WlanSoftmacStartActiveScanResponse::Builder(fidl_arena);
    builder.scan_id(scan_id);
    completer.buffer(arena).ReplySuccess(builder.Build());
  }

  void InstallKey(InstallKeyRequestView request, fdf::Arena& arena,
                  InstallKeyCompleter::Sync& completer) override {
    listener_->WlantapMacSetKey(*request);
    completer.buffer(arena).ReplySuccess();
  }

  void NotifyAssociationComplete(NotifyAssociationCompleteRequestView request, fdf::Arena& arena,
                                 NotifyAssociationCompleteCompleter::Sync& completer) override {
    // This is the test driver, so we can just pretend the association was configured.
    // TODO(fxbug.dev/28907): Evaluate the use and implement
    completer.buffer(arena).ReplySuccess();
  }

  void ClearAssociation(ClearAssociationRequestView request, fdf::Arena& arena,
                        ClearAssociationCompleter::Sync& completer) override {
    // TODO(fxbug.dev/28907): Evaluate the use and implement. Association is never
    // configured, so there is nothing to clear.
    completer.buffer(arena).ReplySuccess();
  }

  void CancelScan(CancelScanRequestView request, fdf::Arena& arena,
                  CancelScanCompleter::Sync& completer) override {
    ZX_PANIC("CancelScan is not supported.");
  }

  void UpdateWmmParameters(UpdateWmmParametersRequestView request, fdf::Arena& arena,
                           UpdateWmmParametersCompleter::Sync& completer) override {
    ZX_PANIC("UpdateWmmParameters is not supported.");
  }

  // WlantapMac impl

  virtual void Rx(const fidl::VectorView<uint8_t>& data,
                  const wlan_tap::WlanRxInfo& rx_info) override {
    std::lock_guard<std::mutex> guard(lock_);

    auto rx_flags = wlan_softmac::WlanRxInfoFlags::TruncatingUnknown(rx_info.rx_flags);
    auto valid_fields = wlan_softmac::WlanRxInfoValid::TruncatingUnknown(rx_info.valid_fields);
    wlan_softmac::WlanRxInfo converted_info = {.rx_flags = rx_flags,
                                               .valid_fields = valid_fields,
                                               .phy = rx_info.phy,
                                               .data_rate = rx_info.data_rate,
                                               .channel = rx_info.channel,
                                               .mcs = rx_info.mcs,
                                               .rssi_dbm = rx_info.rssi_dbm,
                                               .snr_dbh = rx_info.snr_dbh};
    wlan_softmac::WlanRxPacket rx_packet = {.mac_frame = data, .info = converted_info};
    auto arena = fdf::Arena::Create(0, 0);
    auto result = wlan_softmac_ifc_client_.sync().buffer(*arena)->Recv(rx_packet);
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send rx frames up. Status: %d\n", result.status());
    }
  }

  virtual void ReportTxStatus(const wlan_common::WlanTxStatus& ts) override {
    std::lock_guard<std::mutex> guard(lock_);
    auto arena = fdf::Arena::Create(0, 0);
    auto result = wlan_softmac_ifc_client_.sync().buffer(*arena)->ReportTxStatus(ts);
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to report tx status up. Status: %d\n", result.status());
    }
  }

  virtual void ScanComplete(uint64_t scan_id, int32_t status) override {
    std::lock_guard<std::mutex> guard(lock_);
    auto arena = fdf::Arena::Create(0, 0);
    fidl::Arena fidl_arena;
    auto builder =
        fuchsia_wlan_softmac::wire::WlanSoftmacIfcNotifyScanCompleteRequest::Builder(fidl_arena);
    builder.scan_id(scan_id);
    builder.status(status);
    auto result =
        wlan_softmac_ifc_client_.sync().buffer(*arena)->NotifyScanComplete(builder.Build());
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send scan complete notification up. Status: %d\n", result.status());
    }
  }

  virtual void RemoveDevice() override { DdkAsyncRemove(); }

  zx_status_t ServeWlanSoftmacProtocol(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
    auto protocol = [this](fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmac> server_end) mutable {
      if (serving_wlan_softmac_instance_) {
        zxlogf(ERROR, "Cannot bind WlanSoftmac server: Already serving WlanSoftmac");
        return;
      }

      serving_wlan_softmac_instance_ = true;
      fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(server_end), this);
    };

    fuchsia_wlan_softmac::Service::InstanceHandler handler({.wlan_softmac = std::move(protocol)});
    auto status = outgoing_dir_.AddService<fuchsia_wlan_softmac::Service>(std::move(handler));
    if (status.is_error()) {
      zxlogf(ERROR, "Failed to add service to outgoing directory: %s", status.status_string());
      return status.error_value();
    }
    auto result = outgoing_dir_.Serve(std::move(server_end));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to serve outgoing directory: %s", status.status_string());
      return result.error_value();
    }

    return ZX_OK;
  }

  uint16_t id_;
  wlan_common::WlanMacRole role_;
  std::mutex lock_;
  // The FIDL client to communicate with Wlan device.
  fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmacIfc> wlan_softmac_ifc_client_;

  const std::shared_ptr<const wlan_tap::WlantapPhyConfig> phy_config_;
  Listener* listener_;
  zx::channel sme_channel_;

  // Dispatcher for FIDL client of WlanSoftmacIfc protocol.
  const char* WLAN_SOFTMAC_IFC_DISPATCHER_NAME = "wlan-softmac-ifc-client";
  fdf::Dispatcher wlan_softmac_ifc_dispatcher_;

  // Store unbind txn for async reply.
  std::optional<::ddk::UnbindTxn> unbind_txn_;

  // Serves fuchsia_wlan_softmac::Service.
  fdf::OutgoingDirectory outgoing_dir_;

  bool serving_wlan_softmac_instance_;
};

}  // namespace

zx::result<WlantapMac::Ptr> CreateWlantapMac(
    zx_device_t* parent_phy, const wlan_common::WlanMacRole role,
    const std::shared_ptr<const wlan_tap::WlantapPhyConfig> phy_config,
    WlantapMac::Listener* listener, zx::channel sme_channel) {
  static uint16_t n = 0;
  char name[ZX_MAX_NAME_LEN + 1];
  snprintf(name, sizeof(name), "wlansoftmac-%u", n++);
  std::unique_ptr<WlantapMacImpl> wlan_softmac(
      new WlantapMacImpl(parent_phy, role, phy_config, listener, std::move(sme_channel)));

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "%s: failed to create endpoints: %s", __func__, endpoints.status_string());
    return zx::error(endpoints.status_value());
  }

  auto status = wlan_softmac->ServeWlanSoftmacProtocol(std::move(endpoints->server));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: failed to serve wlan softmac service: %s", __func__,
           zx_status_get_string(status));
    return zx::error(status);
  }
  std::array offers = {
      fuchsia_wlan_softmac::Service::Name,
  };

  // The outgoing directory will only be accessible by the driver that binds to
  // the newly created device.
  status = wlan_softmac->DdkAdd(::ddk::DeviceAddArgs(name)
                                    .set_proto_id(ZX_PROTOCOL_WLAN_SOFTMAC)
                                    .set_runtime_service_offers(offers)
                                    .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not add device: %d", __func__, status);
    return zx::error(status);
  }
  // Transfer ownership to devmgr
  auto deleter = [](WlantapMac* wlantap_mac_ptr) { wlantap_mac_ptr->RemoveDevice(); };
  return zx::ok(WlantapMac::Ptr(wlan_softmac.release(), deleter));
}

}  // namespace wlan
