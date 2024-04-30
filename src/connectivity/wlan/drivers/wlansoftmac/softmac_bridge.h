// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/fidl.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/operation/ethernet.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>

#include <mutex>

#include <wlan/drivers/log.h>

#include "device_interface.h"
#include "softmac_ifc_bridge.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

using InitCompleter = fit::callback<void(zx_status_t status, wlansoftmac_handle_t* rust_handle)>;
using StopCompleter = fit::callback<void()>;

class SoftmacBridge : public fidl::Server<fuchsia_wlan_softmac::WlanSoftmacBridge> {
 public:
  static zx::result<std::unique_ptr<SoftmacBridge>> New(
      std::unique_ptr<fit::callback<void(zx_status_t status)>> completer,
      fit::callback<void(zx_status_t)> sta_shutdown_handler, DeviceInterface* device,
      fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
      std::shared_ptr<std::mutex> ethernet_proxy_lock,
      ddk::EthernetIfcProtocolClient* ethernet_proxy);
  zx_status_t Stop(std::unique_ptr<StopCompleter> completer);
  ~SoftmacBridge() override;

  void Query(QueryCompleter::Sync& completer) final;
  void QueryDiscoverySupport(QueryDiscoverySupportCompleter::Sync& completer) final;
  void QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) final;
  void QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) final;
  void QuerySpectrumManagementSupport(
      QuerySpectrumManagementSupportCompleter::Sync& completer) final;
  void Start(StartRequest& request, StartCompleter::Sync& completer) final;
  void SetChannel(SetChannelRequest& request, SetChannelCompleter::Sync& completer) final;
  void JoinBss(JoinBssRequest& request, JoinBssCompleter::Sync& completer) final;
  void EnableBeaconing(EnableBeaconingRequest& request,
                       EnableBeaconingCompleter::Sync& completer) final;
  void DisableBeaconing(DisableBeaconingCompleter::Sync& completer) final;
  void InstallKey(InstallKeyRequest& request, InstallKeyCompleter::Sync& completer) final;
  void NotifyAssociationComplete(NotifyAssociationCompleteRequest& request,
                                 NotifyAssociationCompleteCompleter::Sync& completer) final;
  void ClearAssociation(ClearAssociationRequest& request,
                        ClearAssociationCompleter::Sync& completer) final;
  void StartPassiveScan(StartPassiveScanRequest& request,
                        StartPassiveScanCompleter::Sync& completer) final;
  void StartActiveScan(StartActiveScanRequest& request,
                       StartActiveScanCompleter::Sync& completer) final;
  void CancelScan(CancelScanRequest& request, CancelScanCompleter::Sync& completer) final;
  void UpdateWmmParameters(UpdateWmmParametersRequest& request,
                           UpdateWmmParametersCompleter::Sync& completer) final;
  zx::result<> EthernetTx(eth::BorrowedOperation<>* op, trace_async_id_t async_id) const;
  static zx_status_t WlanTx(void* ctx, const uint8_t* payload, size_t payload_size);
  static zx_status_t EthernetRx(void* ctx, const uint8_t* payload, size_t payload_size);

 private:
  explicit SoftmacBridge(DeviceInterface* device_interface,
                         fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
                         std::shared_ptr<std::mutex> ethernet_proxy_lock,
                         ddk::EthernetIfcProtocolClient* ethernet_proxy);

  template <typename, typename = void>
  static constexpr bool has_value_type = false;
  template <typename T>
  static constexpr bool has_value_type<T, std::void_t<typename T::value_type>> = true;

  std::unique_ptr<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>
      softmac_bridge_server_;
  fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac> softmac_client_;

  // Mark `softmac_ifc_bridge_` as a mutable member of this class so `Start` can be a const function
  // that lazy-initializes `softmac_ifc_bridge_`. Note that `softmac_ifc_bridge_` is never mutated
  // again until its reset upon the framework calling the unbind hook.
  mutable std::unique_ptr<SoftmacIfcBridge> softmac_ifc_bridge_;

  DeviceInterface* device_interface_;
  wlansoftmac_handle_t* rust_handle_;
  fdf::Dispatcher rust_dispatcher_;

  std::shared_ptr<std::mutex> ethernet_proxy_lock_;
  ddk::EthernetIfcProtocolClient* ethernet_proxy_ __TA_GUARDED(ethernet_proxy_lock_);

  static wlansoftmac_buffer_t IntoRustBuffer(std::unique_ptr<Buffer> buffer);
  wlansoftmac_buffer_provider_ops_t rust_buffer_provider{
      .get_buffer = [](size_t min_capacity) -> wlansoftmac_buffer_t {
        WLAN_LAMBDA_TRACE_DURATION("wlansoftmac_buffer_provider_ops_t.get_buffer");
        // Note: Once Rust MLME supports more than sending WLAN frames this needs
        // to change.
        auto buffer = GetBuffer(min_capacity);
        ZX_DEBUG_ASSERT(buffer != nullptr);
        return IntoRustBuffer(std::move(buffer));
      },
  };
};

}  // namespace wlan::drivers::wlansoftmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_
