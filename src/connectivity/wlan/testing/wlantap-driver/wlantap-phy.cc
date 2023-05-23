// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlantap-phy.h"

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fuchsia/wlan/device/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/debug.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <array>
#include <chrono>
#include <memory>
#include <mutex>

#include <ddktl/device.h>
#include <wlan/common/dispatcher.h>
#include <wlan/common/phy.h>

#include "utils.h"
#include "wlantap-mac.h"

namespace wlan {

namespace wlan_softmac = fuchsia_wlan_softmac::wire;
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

// Serves the fuchsia_wlan_tap::WlantapPhy protocol.
struct WlantapPhy : public fidl::WireServer<fuchsia_wlan_tap::WlantapPhy>, WlantapMac::Listener {
  WlantapPhy(zx_device_t* device, zx::channel user_channel,
             std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config)
      : device_(device),
        phy_config_(phy_config),
        user_binding_loop_(std::make_unique<async::Loop>(&kAsyncLoopConfigNoAttachToCurrentThread)),
        name_("wlantap-phy:" + std::string(phy_config_->name.get())),
        user_binding_(fidl::BindServer(
            user_binding_loop_->dispatcher(),
            fidl::ServerEnd<fuchsia_wlan_tap::WlantapPhy>(std::move(user_channel)), this,
            [this](WlantapPhy* server_impl, fidl::UnbindInfo info,
                   fidl::ServerEnd<fuchsia_wlan_tap::WlantapPhy> server_end) {
              auto name = name_;
              fidl_server_unbound_ = true;

              if (shutdown_called_) {
                zxlogf(INFO, "%s: Unbinding WlantapPhy FIDL server.", name.c_str());
              } else {
                zxlogf(ERROR, "%s: Unbinding WlantapPhy FIDL server before Shutdown() called. %s",
                       name.c_str(), info.FormatDescription().c_str());
              }

              if (report_tx_result_count_) {
                zxlogf(INFO, "Tx Status Reports sent during device lifetime: %zu",
                       report_tx_result_count_);
              }

              zxlogf(INFO, "%s: Removing PHY device asynchronously.", name.c_str());

              // Although WlantapPhy isn't part of the driver framework itself, it schedules the
              // removal of the given device when this is unbound, which should allow Shutdown()
              // to teardown all the sim drivers by calling user_binding_.Unbind().
              device_async_remove(device_);

              zxlogf(INFO, "%s: WlantapPhy FIDL server unbind complete.", name.c_str());
            })) {
    zx_status_t status = this->user_binding_loop_->StartThread("wlantap-phy-loop");
    ZX_ASSERT_MSG(status == ZX_OK, "%s: failed to start FIDL server loop: %d", __func__, status);
  }

  void Unbind() {
    zxlogf(INFO, "%s: Unbinding PHY device.", name_.c_str());
    // This call will be ignored by ServerBindingRef it is has
    // already been called, i.e., in the case that DdkUnbind precedes
    // normal shutdowns.
    std::lock_guard<std::mutex> guard(fidl_server_lock_);
    user_binding_.Unbind();
    zxlogf(INFO, "%s: PHY device unbind complete.", name_.c_str());
  }

  void Release() {
    zxlogf(INFO, "%s: Releasing PHY device.", name_.c_str());
    // Flush any remaining tasks in the event loop before destroying the iface.
    // Placed in a block to avoid m, lk, and cv from unintentionally escaping
    // their specific use here.
    {
      std::mutex m;
      std::unique_lock lk(m);
      std::condition_variable cv;
      ::async::PostTask(user_binding_loop_->dispatcher(), [&lk, &cv]() mutable {
        lk.unlock();
        cv.notify_one();
      });
      auto status = cv.wait_for(lk, kFidlServerShutdownTimeout);
      if (status == std::cv_status::timeout) {
        zxlogf(ERROR, "%s: timed out waiting for FIDL server dispatcher to complete.",
               name_.c_str());
        zxlogf(WARNING, "%s: Deleting wlansoftmac devices while FIDL server dispatcher running.",
               name_.c_str());
      }
    }

    std::lock_guard<std::mutex> guard(wlantap_mac_lock_);
    wlantap_mac_.reset();

    delete this;
    zxlogf(INFO, "%s: Release done", name_.c_str());
  }

  // Helpers used by WlanPhyImplDevice
  bool HasWlantapMac() const {
    std::lock_guard<std::mutex> guard(wlantap_mac_lock_);
    return wlantap_mac_ != nullptr;
  }

  zx_status_t SetWlantapMac(WlantapMac::Ptr wlantap_mac) {
    std::lock_guard<std::mutex> guard(wlantap_mac_lock_);
    if (wlantap_mac_ != nullptr) {
      return ZX_ERR_NOT_SUPPORTED;
    }
    wlantap_mac_ = std::move(wlantap_mac);
    return ZX_OK;
  }

  zx_status_t DestroyWlantapMac() {
    std::lock_guard<std::mutex> guard(wlantap_mac_lock_);
    if (wlantap_mac_ == nullptr) {
      return ZX_ERR_NOT_FOUND;
    }
    wlantap_mac_.reset();
    return ZX_OK;
  }

  zx_status_t SetCountry(wlan_tap::SetCountryArgs args) {
    std::lock_guard<std::mutex> guard(fidl_server_lock_);

    fidl::Status status = fidl::WireSendEvent(user_binding_)->SetCountry(args);
    if (!status.ok()) {
      zxlogf(ERROR, "%s: SetCountry() failed: user_binding not bound", status.status_string());
      return status.status();
    }
    return ZX_OK;
  }

  // wlan_tap::WlantapPhy impl

  void Shutdown(ShutdownCompleter::Sync& completer) override {
    zxlogf(INFO, "%s: Shutdown", name_.c_str());
    std::lock_guard<std::mutex> guard(fidl_server_lock_);

    if (shutdown_called_) {
      zxlogf(WARNING, "%s: PHY device shutdown already initiated.", name_.c_str());
      completer.Reply();
      return;
    }
    shutdown_called_ = true;

    zxlogf(INFO, "%s: PHY device shutdown initiated.", name_.c_str());
    user_binding_.Unbind();
    completer.Reply();
  }

  void Rx(RxRequestView request, RxCompleter::Sync& completer) override {
    zxlogf(INFO, "%s: Rx(%zu bytes)", name_.c_str(), request->data.count());
    std::lock_guard<std::mutex> guard(wlantap_mac_lock_);
    if (!wlantap_mac_) {
      zxlogf(ERROR, "No WlantapMac present.");
      return;
    }
    wlantap_mac_->Rx(request->data, request->info);
    zxlogf(DEBUG, "%s: Rx done", name_.c_str());
  }

  void ReportTxResult(ReportTxResultRequestView request,
                      ReportTxResultCompleter::Sync& completer) override {
    std::lock_guard<std::mutex> guard(wlantap_mac_lock_);
    if (!phy_config_->quiet || report_tx_result_count_ < 32) {
      zxlogf(INFO, "%s: ReportTxResult %zu", name_.c_str(), report_tx_result_count_);
    }

    if (!wlantap_mac_) {
      zxlogf(ERROR, "No WlantapMac present.");
      return;
    }

    ++report_tx_result_count_;
    wlantap_mac_->ReportTxResult(request->txr);
    zxlogf(DEBUG, "%s: ScanComplete done", name_.c_str());
    if (!phy_config_->quiet || report_tx_result_count_ <= 32) {
      zxlogf(DEBUG, "%s: ReportTxResult %zu done", name_.c_str(), report_tx_result_count_);
    }
  }

  virtual void ScanComplete(ScanCompleteRequestView request,
                            ScanCompleteCompleter::Sync& completer) override {
    zxlogf(INFO, "%s: ScanComplete(%u)", name_.c_str(), request->status);
    std::lock_guard<std::mutex> guard(wlantap_mac_lock_);
    if (!wlantap_mac_) {
      zxlogf(ERROR, "No WlantapMac present.");
      return;
    }
    wlantap_mac_->ScanComplete(request->scan_id, request->status);
    zxlogf(DEBUG, "%s: ScanComplete done", name_.c_str());
  }

  // WlantapMac::Listener impl

  virtual void WlantapMacStart() override {
    zxlogf(INFO, "%s: WlantapMacStart", name_.c_str());
    std::lock_guard<std::mutex> guard(fidl_server_lock_);
    if (fidl_server_unbound_) {
      return;
    }
    fidl::Status status = fidl::WireSendEvent(user_binding_)->WlanSoftmacStart();
    if (!status.ok()) {
      zxlogf(ERROR, "%s: WlanSoftmacStart() failed", status.status_string());
      return;
    }

    zxlogf(INFO, "%s: WlantapMacStart done", name_.c_str());
  }

  virtual void WlantapMacStop() override { zxlogf(INFO, "%s: WlantapMacStop", name_.c_str()); }

  virtual void WlantapMacQueueTx(const fuchsia_wlan_softmac::wire::WlanTxPacket& pkt) override {
    size_t pkt_size = pkt.mac_frame.count();
    if (!phy_config_->quiet || report_tx_result_count_ < 32) {
      zxlogf(INFO, "%s: WlantapMacQueueTx, size=%zu, tx_report_count=%zu", name_.c_str(), pkt_size,
             report_tx_result_count_);
    }

    std::lock_guard<std::mutex> guard(fidl_server_lock_);
    if (fidl_server_unbound_) {
      zxlogf(INFO, "%s: WlantapMacQueueTx ignored, shutting down", name_.c_str());
      return;
    }

    fidl::Status status = fidl::WireSendEvent(user_binding_)->Tx(ToTxArgs(pkt));
    if (!status.ok()) {
      zxlogf(ERROR, "%s: Tx() failed", status.status_string());
      return;
    }
    if (!phy_config_->quiet || report_tx_result_count_ < 32) {
      zxlogf(DEBUG, "%s: WlantapMacQueueTx done(%zu bytes), tx_report_count=%zu", name_.c_str(),
             pkt_size, report_tx_result_count_);
    }
  }

  virtual void WlantapMacSetChannel(const wlan_common::WlanChannel& channel) override {
    if (!phy_config_->quiet) {
      zxlogf(INFO, "%s: WlantapMacSetChannel channel=%u", name_.c_str(), channel.primary);
    }
    std::lock_guard<std::mutex> guard(fidl_server_lock_);
    if (fidl_server_unbound_) {
      zxlogf(INFO, "%s: WlantapMacSetChannel ignored, shutting down", name_.c_str());
      return;
    }

    fidl::Status status = fidl::WireSendEvent(user_binding_)->SetChannel({.channel = channel});
    if (!status.ok()) {
      zxlogf(ERROR, "%s: SetChannel() failed", status.status_string());
      return;
    }

    if (!phy_config_->quiet) {
      zxlogf(DEBUG, "%s: WlantapMacSetChannel done", name_.c_str());
    }
  }

  virtual void WlantapMacJoinBss(const wlan_common::JoinBssRequest& config) override {
    zxlogf(INFO, "%s: WlantapMacJoinBss", name_.c_str());
    std::lock_guard<std::mutex> guard(fidl_server_lock_);
    if (fidl_server_unbound_) {
      zxlogf(INFO, "%s: WlantapMacJoinBss ignored, shutting down", name_.c_str());
      return;
    }

    fidl::Status status = fidl::WireSendEvent(user_binding_)->JoinBss({.config = config});
    if (!status.ok()) {
      zxlogf(ERROR, "%s: JoinBss() failed", status.status_string());
      return;
    }

    zxlogf(DEBUG, "%s: WlantapMacJoinBss done", name_.c_str());
  }

  virtual void WlantapMacStartScan(const uint64_t scan_id) override {
    zxlogf(INFO, "%s: WlantapMacStartScan", name_.c_str());
    std::lock_guard<std::mutex> guard(fidl_server_lock_);
    if (fidl_server_unbound_) {
      zxlogf(INFO, "%s: WlantapMacStartScan ignored, shutting down", name_.c_str());
      return;
    }

    fidl::Status status = fidl::WireSendEvent(user_binding_)
                              ->StartScan({
                                  .scan_id = scan_id,
                              });
    if (!status.ok()) {
      zxlogf(ERROR, "%s: StartScan() failed", status.status_string());
      return;
    }
    zxlogf(INFO, "%s: WlantapMacStartScan done", name_.c_str());
  }

  virtual void WlantapMacSetKey(const wlan_softmac::WlanKeyConfiguration& key_config) override {
    zxlogf(INFO, "%s: WlantapMacSetKey", name_.c_str());
    std::lock_guard<std::mutex> guard(fidl_server_lock_);
    if (fidl_server_unbound_) {
      zxlogf(INFO, "%s: WlantapMacSetKey ignored, shutting down", name_.c_str());
      return;
    }

    fidl::Status status = fidl::WireSendEvent(user_binding_)->SetKey(ToSetKeyArgs(key_config));
    if (!status.ok()) {
      zxlogf(ERROR, "%s: SetKey() failed", status.status_string());
      return;
    }

    zxlogf(DEBUG, "%s: WlantapMacSetKey done", name_.c_str());
  }

  zx_device_t* device_;
  const std::shared_ptr<const wlan_tap::WlantapPhyConfig> phy_config_;
  std::unique_ptr<async::Loop> user_binding_loop_;
  mutable std::mutex wlantap_mac_lock_;
  WlantapMac::Ptr wlantap_mac_ __TA_GUARDED(wlantap_mac_lock_);
  std::string name_;
  std::mutex fidl_server_lock_;
  fidl::ServerBindingRef<fuchsia_wlan_tap::WlantapPhy> user_binding_
      __TA_GUARDED(fidl_server_lock_);
  bool fidl_server_unbound_ = false;
  const std::chrono::seconds kFidlServerShutdownTimeout = std::chrono::seconds(1);
  bool shutdown_called_ = false;
  size_t report_tx_result_count_ = 0;
};

class WlanPhyImplDevice
    : public ::ddk::Device<WlanPhyImplDevice, ::ddk::Initializable, ::ddk::Unbindable>,
      public fdf::WireServer<fuchsia_wlan_phyimpl::WlanPhyImpl> {
 public:
  WlanPhyImplDevice(zx_device_t* parent, zx::channel user_channel,
                    std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config)
      : ::ddk::Device<WlanPhyImplDevice, ::ddk::Initializable, ::ddk::Unbindable>(parent),
        phy_config_(std::move(phy_config)),
        outgoing_dir_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())),
        user_channel_(std::move(user_channel)) {
    auto dispatcher =
        fdf::SynchronizedDispatcher::Create({}, "wlanphy-impl-server", [&](fdf_dispatcher_t*) {
          if (unbind_txn_)
            unbind_txn_->Reply();
        });
    server_dispatcher_ = std::move(*dispatcher);
    driver_async_dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  }

  ~WlanPhyImplDevice() override = default;

  void DdkInit(::ddk::InitTxn txn) {
    wlantap_phy_ = std::make_unique<WlantapPhy>(zxdev(), std::move(user_channel_), phy_config_);
    txn.Reply(ZX_OK);
  }

  void DdkUnbind(::ddk::UnbindTxn txn) {
    unbind_txn_ = std::move(txn);
    wlantap_phy_->Unbind();
  }

  void DdkRelease() {
    wlantap_phy_->Release();
    delete this;
  }

  zx_status_t ServeWlanPhyImplProtocol(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
    // This callback will be invoked when this service is being connected.
    auto protocol = [this](fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) mutable {
      fdf::BindServer(server_dispatcher_.get(), std::move(server_end), this);
    };

    // Register the callback to handler.
    fuchsia_wlan_phyimpl::Service::InstanceHandler handler({.wlan_phy_impl = std::move(protocol)});

    // Add this service to the outgoing directory so that the child driver can connect to by calling
    // DdkConnectRuntimeProtocol().
    auto status = outgoing_dir_.AddService<fuchsia_wlan_phyimpl::Service>(std::move(handler));
    if (status.is_error()) {
      zxlogf(ERROR, "Failed to add service to outgoing directory: %s\n", status.status_string());
      return status.error_value();
    }

    // Serve the outgoing directory to the entity that intends to open it, which is DFv1 in this
    // case.
    auto result = outgoing_dir_.Serve(std::move(server_end));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to serve outgoing directory: %s\n", result.status_string());
      return result.error_value();
    }

    return ZX_OK;
  }

  void GetSupportedMacRoles(fdf::Arena& arena,
                            GetSupportedMacRolesCompleter::Sync& completer) override {
    // wlantap-phy only supports a single mac role determined by the config
    wlan_common::WlanMacRole supported[1] = {phy_config_->mac_role};
    auto reply_vec = fidl::VectorView<wlan_common::WlanMacRole>::FromExternal(supported, 1);

    zxlogf(INFO, "%s: received a 'GetSupportedMacRoles' DDK request. Responding with roles = {%u}",
           name_.c_str(), static_cast<uint32_t>(phy_config_->mac_role));

    fidl::Arena fidl_arena;
    auto response =
        fuchsia_wlan_phyimpl::wire::WlanPhyImplGetSupportedMacRolesResponse::Builder(fidl_arena)
            .supported_mac_roles(reply_vec)
            .Build();
    completer.buffer(arena).ReplySuccess(response);
  }

  void CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                   CreateIfaceCompleter::Sync& completer) override {
    zxlogf(INFO, "%s: received a 'CreateIface' DDK request", name_.c_str());
    if (wlantap_phy_->HasWlantapMac()) {
      zxlogf(ERROR, "%s: CreateIface: only 1 iface supported per WlantapPhy", name_.c_str());
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
      return;
    }

    auto role_str = RoleToString(request->role());
    zxlogf(INFO, "%s: received a 'CreateIface' for role: %s", name_.c_str(), role_str.c_str());
    if (phy_config_->mac_role != request->role()) {
      zxlogf(ERROR, "%s: CreateIface(%s): role not supported", name_.c_str(), role_str.c_str());
      completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }

    if (!request->mlme_channel().is_valid()) {
      zxlogf(ERROR, "%s: CreateIface(%s): MLME channel in request is invalid", name_.c_str(),
             role_str.c_str());
      completer.buffer(arena).ReplyError(ZX_ERR_IO_INVALID);
      return;
    }

    zx::result<WlantapMac::Ptr> result;
    libsync::Completion served;

    // CreateIface runs on the server dispatcher, but WlantapMac must be created on the driver
    // runtime dispatcher.
    async::PostTask(driver_async_dispatcher_, [&]() {
      result = CreateWlantapMac(parent(), request->role(), phy_config_, wlantap_phy_.get(),
                                std::move(request->mlme_channel()));
      served.Signal();
    });

    constexpr zx::duration kCreateWlantapMacTimeout{ZX_SEC(10)};
    if (served.Wait(kCreateWlantapMacTimeout) != ZX_OK) {
      zxlogf(ERROR, "%s: CreateIface(%s): CreateWlantapMac timed out", name_.c_str(),
             role_str.c_str());
      completer.buffer(arena).ReplyError(ZX_ERR_TIMED_OUT);
      return;
    }

    if (result.is_error()) {
      zxlogf(ERROR, "%s: CreateIface(%s): Failed because %s", name_.c_str(), role_str.c_str(),
             result.status_string());

      completer.buffer(arena).ReplyError(result.status_value());
      return;
    }

    // Transfer ownership of wlantap_mac_ptr to wlantap_phy_
    zx_status_t status = wlantap_phy_->SetWlantapMac(std::move(result.value()));
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: CreateIface(%s): Could not set WlantapMac: %s", name_.c_str(),
             role_str.c_str(), zx_status_get_string(status));
      completer.buffer(arena).ReplyError(status);
      return;
    }

    zxlogf(INFO, "%s: CreateIface(%s): success", name_.c_str(), role_str.c_str());

    fidl::Arena fidl_arena;
    auto resp = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena)
                    .iface_id(0)
                    .Build();

    completer.buffer(arena).ReplySuccess(resp);
  }

  void DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                    DestroyIfaceCompleter::Sync& completer) override {
    zxlogf(INFO, "%s: received a 'DestroyIface' DDK request", name_.c_str());
    if (!wlantap_phy_->HasWlantapMac()) {
      zxlogf(ERROR, "%s: DestroyIface: no iface exists", name_.c_str());
      completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }

    zx_status_t status = wlantap_phy_->DestroyWlantapMac();
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: DestroyIface: Could not destroy WlantapMac: %s ", name_.c_str(),
             zx_status_get_string(status));
      completer.buffer(arena).ReplyError(status);
      return;
    }

    zxlogf(DEBUG, "%s: DestroyIface: done", name_.c_str());
    completer.buffer(arena).ReplySuccess();
  }

  void SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                  SetCountryCompleter::Sync& completer) override {
    zxlogf(INFO, "%s: SetCountry() to [%s] received", name_.c_str(),
           wlan::common::Alpha2ToStr(request->alpha2()).c_str());

    wlan_tap::SetCountryArgs args{.alpha2 = request->alpha2()};
    zx_status_t status = wlantap_phy_->SetCountry(args);
    if (status != ZX_OK) {
      zxlogf(ERROR, "SetCountry() failed: %s", zx_status_get_string(status));
      completer.buffer(arena).ReplyError(status);
      return;
    }
    completer.buffer(arena).ReplySuccess();
  }

  void ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) override {
    zxlogf(WARNING, "%s: ClearCountry() not supported", name_.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) override {
    zxlogf(WARNING, "%s: GetCountry() not supported", name_.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                        SetPowerSaveModeCompleter::Sync& completer) override {
    zxlogf(WARNING, "%s: SetPowerSaveMode() not supported", name_.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetPowerSaveMode(fdf::Arena& arena, GetPowerSaveModeCompleter::Sync& completer) override {
    zxlogf(WARNING, "%s: GetPowerSaveMode() not supported", name_.c_str());
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  const std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config_{};

  // The FIDL server end dispatcher for fuchsia_wlan_wlanphyimpl::WlanPhyImpl protocol.
  // All of the overridden FIDL functions run on this dispatcher.
  fdf::SynchronizedDispatcher server_dispatcher_;

  // Serves fuchsia_wlan_phyimpl::Service.
  fdf::OutgoingDirectory outgoing_dir_;

  // The pointer of the default driver dispatcher in form of async_dispatcher_t.
  // This is needed because calls to CreateIface need to create new drivers, which need to be on
  // the default driver dispatcher.
  async_dispatcher_t* driver_async_dispatcher_;

  // The UnbindTxn provided by driver framework. It's used to call Reply() to synchronize the
  // shutdown of server_dispatcher.
  std::optional<::ddk::UnbindTxn> unbind_txn_;

  // The channel which will be passed to wlantap_phy_ on initialization.
  zx::channel user_channel_{};

  // An instance of WlantapPhy which serves the fuchsia_wlan_tap::WlantapPhy protocol.
  // This will be created on device init because this requires the zx_device_t that gets set when
  // the device is added.
  std::unique_ptr<WlantapPhy> wlantap_phy_{nullptr};
};

}  // namespace

zx_status_t CreatePhy(zx_device_t* wlantapctl, zx::channel user_channel,
                      std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config) {
  zxlogf(INFO, "Creating phy");
  auto phy = std::make_unique<WlanPhyImplDevice>(wlantapctl, std::move(user_channel), phy_config);

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  zx_status_t status = phy->ServeWlanPhyImplProtocol(std::move(endpoints->server));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to serve protocol: %s", zx_status_get_string(status));
    return status;
  }

  std::array<const char*, 1> offers{
      fuchsia_wlan_phyimpl::Service::Name,
  };

  status = phy->DdkAdd(::ddk::DeviceAddArgs(phy_config->name.get().data())
                           .set_proto_id(ZX_PROTOCOL_WLANPHY_IMPL)
                           .set_runtime_service_offers(offers)
                           .set_outgoing_dir(endpoints->client.TakeChannel()));

  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
    return status;
  }

  // Transfer ownership to devmgr
  phy.release();

  zxlogf(INFO, "Phy successfully created");
  return ZX_OK;
}

}  // namespace wlan
