// Copyright (c) 2022 The Fuchsia Authors
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
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_WLAN_INTERFACE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_WLAN_INTERFACE_H_

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <fuchsia/wlan/fullmac/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fit/function.h>
#include <zircon/types.h>

#include <mutex>

#include <ddktl/device.h>
#include <wlan/drivers/components/frame.h>
#include <wlan/drivers/components/network_port.h>

#include "src/connectivity/wlan/drivers/lib/fullmac_ifc/wlan_fullmac_ifc.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/client_connection.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/key_ring.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/scanner.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/softap.h"

namespace wlan::nxpfmac {

struct DeviceContext;
class WlanInterface;
using WlanInterfaceDeviceType = ::ddk::Device<WlanInterface>;

class WlanInterface : public WlanInterfaceDeviceType,
                      public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImpl>,
                      public ClientConnectionIfc,
                      public SoftApIfc,
                      public wlan::drivers::components::NetworkPort,
                      public wlan::drivers::components::NetworkPort::Callbacks {
 public:
  // Static factory function.  The returned instance is unowned, since its lifecycle is managed by
  // the devhost.
  static zx_status_t Create(zx_device_t* parent, uint32_t iface_index,
                            fuchsia_wlan_common::wire::WlanMacRole role, DeviceContext* context,
                            const uint8_t mac_address[ETH_ALEN], zx::channel&& mlme_channel,
                            WlanInterface** out_interface);

  // Initiate an async remove call. The provided callback will be called once DdkRelease is called
  // as part of the removal. Note that when `on_remove` is called the WlanInterface object is
  // already destroyed and should not be referenced.
  void Remove(fit::callback<void()>&& on_remove);

  // Device operations.
  void DdkRelease();

  // Serves the WlanFullmacImpl protocol on `server_end`.
  zx_status_t ServeWlanFullmacImplProtocol(fidl::ServerEnd<fuchsia_io::Directory> server_end);

  void OnEapolResponse(wlan::drivers::components::Frame&& frame);
  void OnEapolTransmitted(zx_status_t status, const uint8_t* dst_addr);

  // WlanFullmacImpl implementations, dispatching FIDL requests from wlanif driver.
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

  // ClientConnectionIfc implementation.
  void OnDisconnectEvent(uint16_t reason_code) override;
  void SignalQualityIndication(int8_t rssi, int8_t snr) override;
  void InitiateSaeHandshake(const uint8_t* peer_mac) override;
  void OnAuthFrame(const uint8_t* peer_mac, const wlan::Authentication* frame,
                   cpp20::span<const uint8_t> trailing_data) override;

  // SoftApIfc implementation.
  void OnStaConnectEvent(uint8_t* sta_mac_addr, uint8_t* ies, uint32_t ie_length) override;
  void OnStaDisconnectEvent(uint8_t* sta_mac_addr, uint16_t reason_code,
                            bool locally_initiated) override;

  // NetworkPort::Callbacks implementation
  uint32_t PortGetMtu() override;
  void MacGetAddress(mac_address_t* out_mac) override;
  void MacGetFeatures(features_t* out_features) override;
  void MacSetMode(mac_filter_mode_t mode, cpp20::span<const mac_address_t> multicast_macs) override;

 private:
  explicit WlanInterface(zx_device_t* parent, uint32_t iface_index,
                         fuchsia_wlan_common::wire::WlanMacRole role, DeviceContext* context,
                         const uint8_t mac_address[ETH_ALEN], zx::channel&& mlme_channel);

  zx_status_t SetMacAddressInFw();
  void ConfirmDeauth();
  void ConfirmDisassoc(zx_status_t status);
  void ConfirmConnectReq(ClientConnection::StatusCode status, const uint8_t* ies = nullptr,
                         size_t ies_len = 0);

  fuchsia_wlan_common::wire::WlanMacRole role_;
  uint32_t iface_index_;
  zx::channel mlme_channel_;

  fit::callback<void()> on_remove_ __TA_GUARDED(mutex_);

  KeyRing key_ring_;
  ClientConnection client_connection_ __TA_GUARDED(mutex_);
  Scanner scanner_ __TA_GUARDED(mutex_);

  SoftAp soft_ap_ __TA_GUARDED(mutex_);
  DeviceContext* context_ = nullptr;

  std::unique_ptr<wlan::WlanFullmacIfc> fullmac_ifc_;

  // Serves fuchsia_wlan_fullmac::Service.
  fdf::OutgoingDirectory outgoing_dir_;

  uint8_t mac_address_[ETH_ALEN] = {};

  bool is_up_ __TA_GUARDED(mutex_) = false;
  std::mutex mutex_;
};

}  // namespace wlan::nxpfmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_WLAN_INTERFACE_H_
