// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.wlan.fullmac/cpp/wire.h>
#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <memory>

#include <sdk/lib/driver/logging/cpp/logger.h>

#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace wlanif {

class Device final : public fdf::DriverBase,
                     public fidl::WireAsyncEventHandler<fdf::NodeController>,
                     public fidl::WireServer<fuchsia_wlan_fullmac::WlanFullmacImplIfc> {
 public:
  explicit Device(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~Device();

  static constexpr const char* Name() { return "wlanif"; }
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  fdf::Logger* Logger() { return logger_.get(); }

  zx_status_t StartFullmacIfcServer(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
                                    zx_handle_t ifc_server_end_handle);

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc>);
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override;

  // Implementation of fuchsia_wlan_fullmac::WlanFullmacImplIfc.
  void OnScanResult(OnScanResultRequestView request,
                    OnScanResultCompleter::Sync& completer) override;
  void OnScanEnd(OnScanEndRequestView request, OnScanEndCompleter::Sync& completer) override;
  void ConnectConf(ConnectConfRequestView request, ConnectConfCompleter::Sync& completer) override;
  void RoamConf(RoamConfRequestView request, RoamConfCompleter::Sync& completer) override;
  void AuthInd(AuthIndRequestView request, AuthIndCompleter::Sync& completer) override;
  void DeauthConf(DeauthConfRequestView request, DeauthConfCompleter::Sync& completer) override;
  void DeauthInd(DeauthIndRequestView request, DeauthIndCompleter::Sync& completer) override;
  void AssocInd(AssocIndRequestView request, AssocIndCompleter::Sync& completer) override;
  void DisassocConf(DisassocConfRequestView request,
                    DisassocConfCompleter::Sync& completer) override;
  void DisassocInd(DisassocIndRequestView request, DisassocIndCompleter::Sync& completer) override;
  void StartConf(StartConfRequestView request, StartConfCompleter::Sync& completer) override;
  void StopConf(StopConfRequestView request, StopConfCompleter::Sync& completer) override;
  void EapolConf(EapolConfRequestView request, EapolConfCompleter::Sync& completer) override;
  void OnChannelSwitch(OnChannelSwitchRequestView request,
                       OnChannelSwitchCompleter::Sync& completer) override;
  void SignalReport(SignalReportRequestView request,
                    SignalReportCompleter::Sync& completer) override;
  void EapolInd(EapolIndRequestView request, EapolIndCompleter::Sync& completer) override;
  void OnPmkAvailable(OnPmkAvailableRequestView request,
                      OnPmkAvailableCompleter::Sync& completer) override;
  void SaeHandshakeInd(SaeHandshakeIndRequestView request,
                       SaeHandshakeIndCompleter::Sync& completer) override;
  void SaeFrameRx(SaeFrameRxRequestView request, SaeFrameRxCompleter::Sync& completer) override;
  void OnWmmStatusResp(OnWmmStatusRespRequestView request,
                       OnWmmStatusRespCompleter::Sync& completer) override;

 protected:
  void Shutdown();

 private:
  using RustFullmacMlme =
      std::unique_ptr<wlan_fullmac_mlme_handle_t, void (*)(wlan_fullmac_mlme_handle_t*)>;

  // Manages the lifetime of the protocol struct we pass down to the vendor driver. Actual
  // calls to this protocol should only be performed by the vendor driver.
  std::unique_ptr<wlan_fullmac_impl_ifc_banjo_protocol_ops_t>
      wlan_fullmac_impl_ifc_banjo_protocol_ops_;
  std::mutex wlan_fullmac_impl_ifc_banjo_protocol_lock_;
  std::unique_ptr<wlan_fullmac_impl_ifc_banjo_protocol_t> wlan_fullmac_impl_ifc_banjo_protocol_
      __TA_GUARDED(wlan_fullmac_impl_ifc_banjo_protocol_lock_);
  std::mutex rust_mlme_lock_;
  RustFullmacMlme rust_mlme_ __TA_GUARDED(rust_mlme_lock_);

  // Dispatcher for being a FIDL server firing replies to WlanIf device
  fdf::Dispatcher server_dispatcher_;

  fidl::WireClient<fdf::Node> parent_node_;
  std::unique_ptr<fdf::Logger> logger_;
};

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
