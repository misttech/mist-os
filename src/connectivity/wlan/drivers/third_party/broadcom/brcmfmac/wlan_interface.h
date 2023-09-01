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

#include <fidl/fuchsia.factory.wlan/cpp/wire.h>
#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/arena.h>
#include <lib/fdf/cpp/channel.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fit/function.h>
#include <zircon/types.h>

#include <memory>
#include <shared_mutex>

#include <ddktl/device.h>
#include <wlan/drivers/components/network_port.h>

#include "lib/fidl_driver/include/lib/fidl_driver/cpp/wire_messaging_declarations.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/core.h"

struct wireless_dev;

namespace wlan {
namespace brcmfmac {

class WlanInterface;

class WlanInterface : public ddk::Device<WlanInterface, ddk::Unbindable>,
                      public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImpl>,
                      public wlan::drivers::components::NetworkPort,
                      public wlan::drivers::components::NetworkPort::Callbacks {
 public:
  // Static factory function.  The returned instance is unowned, since its lifecycle is managed by
  // the devhost.
  static zx_status_t Create(wlan::brcmfmac::Device* device, const char* name, wireless_dev* wdev,
                            wlan_mac_role_t role, WlanInterface** out_interface);

  // Accessors.
  void set_wdev(wireless_dev* wdev);
  wireless_dev* take_wdev();

  // Called by WlanPhyImpl device when destroying the iface.
  void Remove(fit::callback<void()>&& on_remove);

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  // Serves the WlanFullmacImpl protocol on `server_end`.
  zx_status_t ServeWlanFullmacImplProtocol(fidl::ServerEnd<fuchsia_io::Directory> server_end);

  static zx_status_t GetSupportedMacRoles(
      struct brcmf_pub* drvr,
      fuchsia_wlan_common::wire::WlanMacRole
          out_supported_mac_roles_list[fuchsia_wlan_common::wire::kMaxSupportedMacRoles],
      uint8_t* out_supported_mac_roles_count);
  static zx_status_t SetCountry(brcmf_pub* drvr, const wlan_phy_country_t* country);
  // Reads the currently configured `country` from the firmware.
  static zx_status_t GetCountry(brcmf_pub* drvr, wlan_phy_country_t* out_country);
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
  void ReconnectReq(ReconnectReqRequestView request, fdf::Arena& arena,
                    ReconnectReqCompleter::Sync& completer) override;
  void AuthResp(AuthRespRequestView request, fdf::Arena& arena,
                AuthRespCompleter::Sync& completer) override;
  void DeauthReq(DeauthReqRequestView request, fdf::Arena& arena,
                 DeauthReqCompleter::Sync& completer) override;
  void AssocResp(AssocRespRequestView request, fdf::Arena& arena,
                 AssocRespCompleter::Sync& completer) override;
  void DisassocReq(DisassocReqRequestView request, fdf::Arena& arena,
                   DisassocReqCompleter::Sync& completer) override;
  void ResetReq(ResetReqRequestView request, fdf::Arena& arena,
                ResetReqCompleter::Sync& completer) override;
  void StartReq(StartReqRequestView request, fdf::Arena& arena,
                StartReqCompleter::Sync& completer) override;
  void StopReq(StopReqRequestView request, fdf::Arena& arena,
               StopReqCompleter::Sync& completer) override;
  void SetKeysReq(SetKeysReqRequestView request, fdf::Arena& arena,
                  SetKeysReqCompleter::Sync& completer) override;
  void DelKeysReq(DelKeysReqRequestView request, fdf::Arena& arena,
                  DelKeysReqCompleter::Sync& completer) override;
  void EapolReq(EapolReqRequestView request, fdf::Arena& arena,
                EapolReqCompleter::Sync& completer) override;
  void GetIfaceCounterStats(fdf::Arena& arena,
                            GetIfaceCounterStatsCompleter::Sync& completer) override;
  void GetIfaceHistogramStats(fdf::Arena& arena,
                              GetIfaceHistogramStatsCompleter::Sync& completer) override;
  void StartCaptureFrames(StartCaptureFramesRequestView request, fdf::Arena& arena,
                          StartCaptureFramesCompleter::Sync& completer) override;
  void StopCaptureFrames(fdf::Arena& arena, StopCaptureFramesCompleter::Sync& completer) override;
  void SetMulticastPromisc(SetMulticastPromiscRequestView request, fdf::Arena& arena,
                           SetMulticastPromiscCompleter::Sync& completer) override;
  void SaeHandshakeResp(SaeHandshakeRespRequestView request, fdf::Arena& arena,
                        SaeHandshakeRespCompleter::Sync& completer) override;
  void SaeFrameTx(SaeFrameTxRequestView request, fdf::Arena& arena,
                  SaeFrameTxCompleter::Sync& completer) override;
  void WmmStatusReq(fdf::Arena& arena, WmmStatusReqCompleter::Sync& completer) override;
  void OnLinkStateChanged(OnLinkStateChangedRequestView request, fdf::Arena& arena,
                          OnLinkStateChangedCompleter::Sync& completer) override;

 protected:
  // NetworkPort::Callbacks implementation
  uint32_t PortGetMtu() override;
  void MacGetAddress(uint8_t out_mac[6]) override;
  void MacGetFeatures(features_t* out_features) override;
  void MacSetMode(mode_t mode, cpp20::span<const uint8_t> multicast_macs) override;

 private:
  WlanInterface(wlan::brcmfmac::Device* device, const network_device_ifc_protocol_t& proto,
                uint8_t port_id, const char* name);

  std::shared_mutex lock_;
  wireless_dev* wdev_;               // lock_ is used as a RW lock on wdev_
  fit::callback<void()> on_remove_;  // lock_ is also used as a RW lock on on_remove_
  wlan::brcmfmac::Device* device_;

  // Store unbind txn for async reply
  std::optional<::ddk::UnbindTxn> unbind_txn_;

  // Serves fuchsia_wlan_fullmac::Service.
  fdf::OutgoingDirectory outgoing_dir_;
};
}  // namespace brcmfmac
}  // namespace wlan
#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_WLAN_INTERFACE_H_
