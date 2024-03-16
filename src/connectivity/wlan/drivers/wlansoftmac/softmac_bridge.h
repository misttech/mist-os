// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BRIDGE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.softmac/cpp/wire.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fidl_driver/cpp/wire_client.h>
#include <lib/operation/ethernet.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>

#include <mutex>

#include <wlan/drivers/log.h>

#include "device_interface.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

using InitCompleter = fit::callback<void(zx_status_t status, wlansoftmac_handle_t* rust_handle)>;
using StopCompleter = fit::callback<void()>;

class SoftmacBridge : public fidl::WireServer<fuchsia_wlan_softmac::WlanSoftmacBridge> {
 public:
  static zx::result<std::unique_ptr<SoftmacBridge>> New(
      fdf::Dispatcher& softmac_bridge_server_dispatcher,
      std::unique_ptr<fit::callback<void(zx_status_t status)>> completer,
      fit::callback<void(zx_status_t)> sta_shutdown_handler, DeviceInterface* device,
      fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
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
  void SetChannel(SetChannelRequestView request, SetChannelCompleter::Sync& completer) final;
  void JoinBss(JoinBssRequestView request, JoinBssCompleter::Sync& completer) final;
  void EnableBeaconing(EnableBeaconingRequestView request,
                       EnableBeaconingCompleter::Sync& completer) final;
  void DisableBeaconing(DisableBeaconingCompleter::Sync& completer) final;
  void InstallKey(InstallKeyRequestView request, InstallKeyCompleter::Sync& completer) final;
  void NotifyAssociationComplete(NotifyAssociationCompleteRequestView request,
                                 NotifyAssociationCompleteCompleter::Sync& completer) final;
  void ClearAssociation(ClearAssociationRequestView request,
                        ClearAssociationCompleter::Sync& completer) final;
  void StartPassiveScan(StartPassiveScanRequestView request,
                        StartPassiveScanCompleter::Sync& completer) final;
  void StartActiveScan(StartActiveScanRequestView request,
                       StartActiveScanCompleter::Sync& completer) final;
  void CancelScan(CancelScanRequestView request, CancelScanCompleter::Sync& completer) final;
  void UpdateWmmParameters(UpdateWmmParametersRequestView request,
                           UpdateWmmParametersCompleter::Sync& completer) final;
  static zx_status_t WlanTx(const void* ctx, const uint8_t* payload, size_t payload_size);
  static zx_status_t EthernetRx(const void* ctx, const uint8_t* payload, size_t payload_size);

 private:
  explicit SoftmacBridge(DeviceInterface* device_interface,
                         fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
                         std::shared_ptr<std::mutex> ethernet_proxy_lock,
                         ddk::EthernetIfcProtocolClient* ethernet_proxy);

  template <typename, typename = void>
  static constexpr bool has_value_type = false;
  template <typename T>
  static constexpr bool has_value_type<T, std::void_t<typename T::value_type>> = true;

  template <typename FidlMethod>
  static fidl::WireResultUnwrapType<FidlMethod> FlattenAndLogError(
      const std::string& method_name, fdf::WireUnownedResult<FidlMethod>& result);

  template <typename FidlMethod>
  using Dispatcher = std::function<fdf::WireUnownedResult<FidlMethod>(
      const fdf::Arena&, const fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&)>;

  template <typename Completer, typename FidlMethod>
  void DispatchAndComplete(const std::string& method_name, Dispatcher<FidlMethod> dispatcher,
                           Completer& completer);

  std::unique_ptr<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>
      softmac_bridge_server_;
  fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac> softmac_client_;

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
