// Copyright (c) 2019 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_WLAN_INTERFACE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_WLAN_INTERFACE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
// #include <fidl/fuchsia.factory.wlan/cpp/wire.h>
#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/arena.h>
#include <lib/fdf/cpp/channel.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/types.h>

#include <memory>
#include <shared_mutex>

#include <wlan/drivers/components/network_port.h>

#include "lib/fidl_driver/include/lib/fidl_driver/cpp/wire_messaging_declarations.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/core.h"

struct wireless_dev;

namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace wlan {
namespace brcmfmac {

class WlanInterface;

class WlanInterface : public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImpl>,
                      public fidl::WireAsyncEventHandler<fdf::NodeController>,
                      public wlan::drivers::components::NetworkPort,
                      public wlan::drivers::components::NetworkPort::Callbacks {
 public:
  // Static factory function. The result is provided through the |on_complete| callback. The
  // callback may be called inline from this call in case of an error (but not on success).
  // Make sure this does not attempt to recursively acquire any locks.
  static void Create(wlan::brcmfmac::Device* device, const char* name, wireless_dev* wdev,
                     fuchsia_wlan_common_wire::WlanMacRole role, uint16_t iface_id,
                     fit::callback<void(zx::result<std::unique_ptr<WlanInterface>>)>&& on_complete);
  void DestroyIface(fit::callback<void(zx_status_t)>&& on_complete);

  // Accessors.
  void set_wdev(wireless_dev* wdev);
  wireless_dev* take_wdev();
  std::string GetName() { return name_; }

  // Serves the WlanFullmacImpl protocol on `server_end`.
  void ServiceConnectHandler(fdf_dispatcher_t* dispatcher,
                             fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end);
  fuchsia_wlan_common_wire::WlanMacRole Role() { return role_; }

  static zx_status_t GetSupportedMacRoles(
      struct brcmf_pub* drvr,
      fuchsia_wlan_common::wire::WlanMacRole
          out_supported_mac_roles_list[fuchsia_wlan_common::wire::kMaxSupportedMacRoles],
      uint8_t* out_supported_mac_roles_count);
  static zx_status_t SetCountry(brcmf_pub* drvr,
                                const fuchsia_wlan_phyimpl_wire::WlanPhyCountry* country);
  // Reads the currently configured `country` from the firmware.
  static zx_status_t GetCountry(brcmf_pub* drvr, uint8_t* cc_code);
  static zx_status_t ClearCountry(brcmf_pub* drvr);

  // WlanFullmacImpl implementations, dispatching FIDL requests from higher layers.
  void Start(StartRequestView request, fdf::Arena& arena, StartCompleter::Sync& completer) override;
  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer) override;
  void Query(fdf::Arena& arena, QueryCompleter::Sync& completer) override;
  void QueryMacSublayerSupport(fdf::Arena& arena,
                               QueryMacSublayerSupportCompleter::Sync& completer) override;
  void QuerySecuritySupport(fdf::Arena& arena,
                            QuerySecuritySupportCompleter::Sync& completer) override;
  void QuerySpectrumManagementSupport(
      fdf::Arena& arena, QuerySpectrumManagementSupportCompleter::Sync& completer) override;
  void StartScan(StartScanRequestView request, fdf::Arena& arena,
                 StartScanCompleter::Sync& completer) override;
  void Connect(ConnectRequestView request, fdf::Arena& arena,
               ConnectCompleter::Sync& completer) override;
  void Reconnect(ReconnectRequestView request, fdf::Arena& arena,
                 ReconnectCompleter::Sync& completer) override;
  void AuthResp(AuthRespRequestView request, fdf::Arena& arena,
                AuthRespCompleter::Sync& completer) override;
  void Deauth(DeauthRequestView request, fdf::Arena& arena,
              DeauthCompleter::Sync& completer) override;
  void AssocResp(AssocRespRequestView request, fdf::Arena& arena,
                 AssocRespCompleter::Sync& completer) override;
  void Disassoc(DisassocRequestView request, fdf::Arena& arena,
                DisassocCompleter::Sync& completer) override;
  void Reset(ResetRequestView request, fdf::Arena& arena, ResetCompleter::Sync& completer) override;
  void StartBss(StartBssRequestView request, fdf::Arena& arena,
                StartBssCompleter::Sync& completer) override;
  void StopBss(StopBssRequestView request, fdf::Arena& arena,
               StopBssCompleter ::Sync& completer) override;
  void SetKeysReq(SetKeysReqRequestView request, fdf::Arena& arena,
                  SetKeysReqCompleter::Sync& completer) override;
  void DelKeysReq(DelKeysReqRequestView request, fdf::Arena& arena,
                  DelKeysReqCompleter::Sync& completer) override;
  void EapolTx(EapolTxRequestView request, fdf::Arena& arena,
               EapolTxCompleter::Sync& completer) override;
  void GetIfaceCounterStats(fdf::Arena& arena,
                            GetIfaceCounterStatsCompleter::Sync& completer) override;
  void GetIfaceHistogramStats(fdf::Arena& arena,
                              GetIfaceHistogramStatsCompleter::Sync& completer) override;
  void SetMulticastPromisc(SetMulticastPromiscRequestView request, fdf::Arena& arena,
                           SetMulticastPromiscCompleter::Sync& completer) override;
  void SaeHandshakeResp(SaeHandshakeRespRequestView request, fdf::Arena& arena,
                        SaeHandshakeRespCompleter::Sync& completer) override;
  void SaeFrameTx(SaeFrameTxRequestView request, fdf::Arena& arena,
                  SaeFrameTxCompleter::Sync& completer) override;
  void WmmStatusReq(fdf::Arena& arena, WmmStatusReqCompleter::Sync& completer) override;
  void OnLinkStateChanged(OnLinkStateChangedRequestView request, fdf::Arena& arena,
                          OnLinkStateChangedCompleter::Sync& completer) override;

  void on_fidl_error(fidl::UnbindInfo error) override {
    BRCMF_WARN("Fidl Error: %s", error.FormatDescription().c_str());
  }
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override {
    BRCMF_WARN("Received unknown event: event_ordinal(%lu)", metadata.event_ordinal);
  }

 protected:
  // NetworkPort::Callbacks implementation
  uint32_t PortGetMtu() override;
  void MacGetAddress(fuchsia_net::MacAddress* out_mac) override;
  void MacGetFeatures(fuchsia_hardware_network_driver::Features* out_features) override;
  void MacSetMode(fuchsia_hardware_network::wire::MacFilterMode mode,
                  cpp20::span<const ::fuchsia_net::wire::MacAddress> multicast_macs) override;
  void PortRemoved() override;

 private:
  WlanInterface(
      wlan::brcmfmac::Device* device,
      fdf::WireSharedClient<fuchsia_hardware_network_driver::NetworkDeviceIfc>&& netdev_ifc,
      uint8_t port_id, const char* name, uint16_t iface_id);
  zx_status_t AddWlanFullmacDevice();
  zx_status_t RemoveWlanFullmacDevice();

  std::shared_mutex lock_;
  wireless_dev* wdev_ = nullptr;  // lock_ is used as a RW lock on wdev_
  bool destroying_ __TA_GUARDED(lock_) = false;
  wlan::brcmfmac::Device* device_ = nullptr;
  fuchsia_wlan_common_wire::WlanMacRole role_;
  std::string name_;
  // This is the interface ID used by the Device object, not the port ID or firmware ID.
  uint16_t iface_id_;
  fdf::ServerBindingGroup<fuchsia_wlan_fullmac::WlanFullmacImpl> bindings_;
  fidl::WireClient<fdf::NodeController> wlanfullmac_controller_;
};
}  // namespace brcmfmac
}  // namespace wlan
#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_WLAN_INTERFACE_H_
