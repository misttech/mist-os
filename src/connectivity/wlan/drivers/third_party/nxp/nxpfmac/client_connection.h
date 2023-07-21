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
#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_CLIENT_CONNECTION_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_CLIENT_CONNECTION_H_

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.ieee80211/cpp/common_types.h>
#include <fuchsia/hardware/wlan/fullmac/cpp/banjo.h>
#include <lib/stdcompat/span.h>
#include <netinet/if_ether.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <mutex>

#include <wlan/common/mac_frame.h>
#include <wlan/drivers/timer/timer.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/event_handler.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_request.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/waitable_state.h"

namespace wlan::nxpfmac {

class ConnectRequestParams;
struct DeviceContext;
class KeyRing;

class ClientConnectionIfc {
 public:
  virtual ~ClientConnectionIfc() = default;

  virtual void OnDisconnectEvent(uint16_t reason_code) = 0;
  virtual void SignalQualityIndication(int8_t rssi, int8_t snr) = 0;
  virtual void InitiateSaeHandshake(const uint8_t* peer_mac) = 0;
  virtual void OnAuthFrame(const uint8_t* peer_mac, const wlan::Authentication* frame,
                           cpp20::span<const uint8_t> trailing_data) = 0;
};

constexpr size_t kConnectReqBufferSize =
    fidl::MaxSizeInChannel<fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectReqRequest,
                           fidl::MessageDirection::kSending>();

class ClientConnection {
 public:
  using StatusCode = fuchsia_wlan_ieee80211::StatusCode;
  using OnConnectCallback =
      std::function<void(StatusCode, const uint8_t* /*ies*/, size_t /*ies_size*/)>;

  ClientConnection(ClientConnectionIfc* ifc, DeviceContext* context, KeyRing* key_ring,
                   uint32_t bss_index);
  ~ClientConnection();
  // Attempt to connect using the parameters provided in `req`. Returns ZX_ERR_ALREADY_EXISTS if a
  // connection attempt is already in progress. Returns ZX_OK if the request is successfully
  // initiated, `on_connect` will be called asynchronously with the result of the connection
  // attempt.
  zx_status_t Connect(const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectReqRequest* req,
                      OnConnectCallback&& on_connect) __TA_EXCLUDES(mutex_);
  // Cancel a connection attempt. This will call the on_connect callback passed to Connect if a
  // connection attempt was found. Returns ZX_ERR_NOT_FOUND if no connection attempt is in progress.
  zx_status_t CancelConnect() __TA_EXCLUDES(mutex_);

  // Returns ZX_ERR_NOT_CONNECTED if not connected or ZX_ERR_ALREADY_EXISTS if a disconnect attempt
  // is already in progress. Otherwise attempt to disconnect from an established connection.
  zx_status_t Disconnect(const uint8_t* addr, uint16_t reason_code,
                         std::function<void(IoctlStatus)>&& on_disconnect_complete)
      __TA_EXCLUDES(mutex_);

  zx_status_t TransmitAuthFrame(const uint8_t* source_mac, const uint8_t* destination_mac,
                                uint16_t sequence, uint16_t status_code,
                                cpp20::span<const uint8_t> sae_fields) __TA_EXCLUDES(mutex_);
  zx_status_t OnSaeResponse(const uint8_t* peer, uint16_t status_code) __TA_EXCLUDES(mutex_);

 private:
  zx_status_t ConnectLocked(const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectReqRequest* req,
                            OnConnectCallback&& on_connect) __TA_REQUIRES(mutex_);

  zx_status_t OnSaeResponseLocked(const uint8_t* peer, uint16_t status_code) __TA_REQUIRES(mutex_);
  void OnDisconnect(uint16_t reason_code) __TA_EXCLUDES(mutex_);
  void OnManagementFrame(pmlan_event event) __TA_EXCLUDES(mutex_);
  void OnSaeTimeout() __TA_EXCLUDES(mutex_);

  zx_status_t InitiateSaeHandshake(
      const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectReqRequest* req);
  zx_status_t RegisterForMgmtFrames(const std::vector<wlan::ManagementSubtype>& types);
  zx_status_t RemainOnChannel(uint8_t channel);
  zx_status_t CancelRemainOnChannel();
  zx_status_t GetRsnCipherSuites(const uint8_t* ies, size_t ies_count,
                                 uint8_t* out_pairwise_cipher_suite,
                                 uint8_t* out_group_cipher_suite);
  zx_status_t ClearIes();
  zx_status_t ConfigureIes(const uint8_t* ies, size_t ies_count);
  zx_status_t SetAuthMode(fuchsia_wlan_fullmac::wire::WlanAuthType auth_type);
  zx_status_t SetEncryptMode(uint8_t cipher_suite);
  zx_status_t SetWpaEnabled(bool enabled);

  void TriggerConnectCallback(StatusCode status_code, const uint8_t* ies, size_t ies_size)
      __TA_REQUIRES(mutex_);
  void CompleteConnection(StatusCode status_code, const uint8_t* ies = nullptr, size_t ies_size = 0)
      __TA_REQUIRES(mutex_);
  void IndicateSignalQuality();

  enum class State {
    Idle,
    Authenticating,
    Connecting,
    Connected,
    Disconnecting,
  };

  WaitableState<State> state_{State::Idle};

  // Periodic timer to log client stats, etc.
  std::unique_ptr<wlan::drivers::timer::Timer> log_timer_;
  wlan::drivers::timer::Timer sae_timeout_timer_;

  ClientConnectionIfc* ifc_ = nullptr;
  DeviceContext* context_ = nullptr;
  KeyRing* key_ring_ = nullptr;
  const uint32_t bss_index_;

  // These addresses are used during authentication and might not be valid at other times.
  wlan::common::MacAddr client_mac_ __TA_GUARDED(mutex_);
  wlan::common::MacAddr ap_mac_ __TA_GUARDED(mutex_);

  OnConnectCallback on_connect_;
  std::unique_ptr<ConnectRequestParams> connect_request_params_ __TA_GUARDED(mutex_);
  // Something inside mlan_ds_bss makes this a variable size struct so we need to have a pointer.
  // Otherwise it has to be at the end of this class and that makes this class variable size which
  // means that all instances of this class would have to be at the end of any classes containing it
  // and so on.
  std::unique_ptr<IoctlRequest<mlan_ds_bss>> connect_request_ __TA_GUARDED(mutex_);
  std::mutex mutex_;

  EventRegistration disconnect_event_;
  EventRegistration mgmt_frame_event_;
};

}  // namespace wlan::nxpfmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_NXP_NXPFMAC_CLIENT_CONNECTION_H_
