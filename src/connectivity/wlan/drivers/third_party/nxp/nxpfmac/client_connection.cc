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

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/client_connection.h"

#include <fidl/fuchsia.wlan.fullmac/cpp/fidl.h>
#include <fuchsia/wlan/ieee80211/c/banjo.h>
#include <lib/ddk/debug.h>
#include <netinet/if_ether.h>

#include <src/lib/fidl/cpp/include/lib/fidl/cpp/wire_natural_conversions.h>
#include <wlan/common/mac_frame.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/debug.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device_context.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/event_handler.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ies.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_adapter.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/key_ring.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/utils.h"

namespace wlan::nxpfmac {

using wlan::drivers::timer::Timer;
constexpr zx_duration_t kDestructionStateTimeout = ZX_SEC(5);
constexpr zx_duration_t kConnectionTimeout = ZX_MSEC(6000);
constexpr zx_duration_t kDisconnectTimeout = ZX_MSEC(1000);
constexpr zx_duration_t kLogTimerTimeout = ZX_SEC(30);
constexpr zx_duration_t kSaeHandshakeTimeout = ZX_SEC(2);
// Time to wait before a remain on channel request expires
constexpr uint32_t kRemainOnChannelPeriodMillis = 2400;

constexpr uint8_t kSaeFramePriority = 7u;

// A class that allows us to keep a local copy of a connect request, including copying of allocated
// data and updating pointers to that data.
class ConnectRequestParams {
 public:
  explicit ConnectRequestParams(
      const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest* req)
      : request_arena_() {
    auto natural_req = fidl::ToNatural(*req);
    request_ = fidl::ToWire(request_arena_, natural_req);
  }

  const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest* get() const { return &request_; }

 private:
  fidl::Arena<kConnectReqBufferSize> request_arena_;
  fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest request_;
};

ClientConnection::ClientConnection(ClientConnectionIfc* ifc, DeviceContext* context,
                                   KeyRing* key_ring, uint32_t bss_index)
    : sae_timeout_timer_(context->device_->GetDispatcher(), [this] { OnSaeTimeout(); }),
      ifc_(ifc),
      context_(context),
      key_ring_(key_ring),
      bss_index_(bss_index),
      connect_request_(new IoctlRequest<mlan_ds_bss>()) {
  disconnect_event_ = context_->event_handler_->RegisterForInterfaceEvent(
      MLAN_EVENT_ID_FW_DISCONNECTED, bss_index, [this](pmlan_event event) {
        OnDisconnect(*reinterpret_cast<const uint16_t*>(event->event_buf));
      });
  mgmt_frame_event_ = context_->event_handler_->RegisterForInterfaceEvent(
      MLAN_EVENT_ID_DRV_MGMT_FRAME, bss_index,
      [this](pmlan_event event) { OnManagementFrame(event); });
  log_timer_ = std::make_unique<Timer>(context_->device_->GetDispatcher(),
                                       [this]() { IndicateSignalQuality(); });
}

ClientConnection::~ClientConnection() {
  // Stop the periodic log timer unconditionally.
  log_timer_->Stop();
  // Cancel any SAE authentication in progress.
  OnSaeResponse(nullptr, STATUS_CODE_CANCELED);
  // Cancel any ongoing connection attempt.
  zx_status_t status = CancelConnect();
  if (status != ZX_OK && status != ZX_ERR_NOT_FOUND) {
    NXPF_ERR("Failed to cancel connection: %s", zx_status_get_string(status));
    // Don't attempt to wait for the correct state here, it might never happen.
    return;
  }

  // Using a MAC address of all zeroes will disconnect from the currently connected BSSID.
  constexpr uint8_t kZeroMac[ETH_ALEN] = {};
  status = Disconnect(kZeroMac, REASON_CODE_LEAVING_NETWORK_DEAUTH, [this](IoctlStatus io_status) {
    if (io_status != IoctlStatus::Success) {
      // Because we're waiting for the disconnect to complete in the destructor we should signal
      // that the state is idle anyway. This allows the destructor to complete instead of
      // permanently lock up.
      NXPF_ERR("Failed to disconnect, destroying connection anyway: %d", io_status);
      std::lock_guard lock(mutex_);
      state_ = State::Idle;
    }
  });
  if (status != ZX_OK && status != ZX_ERR_NOT_CONNECTED && status != ZX_ERR_ALREADY_EXISTS) {
    NXPF_ERR("Failed to disconnect: %s", zx_status_get_string(status));
    // Don't attempt to wait for the disconnected state here, it might never happen.
    return;
  }

  // Wait until any connect or disconnect attempt has completed. A connect or disconnect attempt
  // will cause asynchronous callbacks that could crash if the connection object goes away. By
  // waiting for an attempt to finish we avoid this. The CancelConnect call above should immediately
  // try to stop any ongoing connection attempt. Disconnect attempts should complete fast enough
  // that this shouldn't be an issue.
  std::unique_lock lock(mutex_);
  if (!state_.WaitFor(lock, State::Idle, kDestructionStateTimeout)) {
    NXPF_ERR("Connection failed to reach idle state at destruction");
  }
}

zx_status_t ClientConnection::Connect(
    const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest* req,
    OnConnectCallback&& on_connect) {
  std::lock_guard lock(mutex_);
  if (state_ != State::Idle && state_ != State::Authenticating) {
    NXPF_ERR("Invalid state for connect call: %d", state_.Load());
    return ZX_ERR_BAD_STATE;
  }

  const bool is_sae = req->auth_type() == fuchsia_wlan_fullmac::wire::WlanAuthType::kSae;
  if (is_sae) {
    zx_status_t status = InitiateSaeHandshake(req);
    if (status != ZX_OK) {
      NXPF_ERR("Failed to initiate SAE handshake: %s", zx_status_get_string(status));
      return status;
    }
    // This kicks off an asynchronous process that will eventually complete the connection. At that
    // point we will call the on_connect callback.
    on_connect_ = std::move(on_connect);
    connect_request_params_ = std::make_unique<ConnectRequestParams>(req);
    state_ = State::Authenticating;
    status = sae_timeout_timer_.StartOneshot(kSaeHandshakeTimeout);
    if (status != ZX_OK) {
      NXPF_ERR("Cannot start SAE timeout timer: %s", zx_status_get_string(status));
      return status;
    }
    return ZX_OK;
  }

  return ConnectLocked(req, std::move(on_connect));
}

zx_status_t ClientConnection::ConnectLocked(
    const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest* req,
    OnConnectCallback&& on_connect) {
  if (!req->has_selected_bss() || !req->has_auth_type()) {
    NXPF_ERR("Missing field in connection request: %s %s",
             req->has_selected_bss() ? "" : "selected_bss",
             req->has_auth_type() ? "" : "auth_type");
    return ZX_ERR_INVALID_ARGS;
  }

  auto ssid = IeView(req->selected_bss().ies.data(), req->selected_bss().ies.count()).get(SSID);
  if (!ssid) {
    NXPF_ERR("Missing SSID in connection request");
    return ZX_ERR_INVALID_ARGS;
  }
  if (ssid->size() > MLAN_MAX_SSID_LENGTH) {
    NXPF_ERR("SSID length of %zu exceeds max length %u", ssid->size(), MLAN_MAX_SSID_LENGTH);
    return ZX_ERR_INVALID_ARGS;
  }

  zx_status_t status = key_ring_->RemoveAllKeys();
  if (status != ZX_OK) {
    NXPF_ERR("Could not remove all keys: %s", zx_status_get_string(status));
    return status;
  }

  if (req->has_wep_key() && req->wep_key().key.count() > 0) {
    // The WEP key address should be considered a group key, set address to broadcast address.
    fuchsia_wlan_fullmac::wire::SetKeyDescriptor& wep_key = req->wep_key();
    memset(wep_key.address.data(), 0xFF, ETH_ALEN);
    status = key_ring_->AddKey(wep_key);
    if (status != ZX_OK) {
      NXPF_ERR("Could not set WEP key: %s", zx_status_get_string(status));
      return status;
    }
    status = key_ring_->EnableWepKey(wep_key.key_id);
    if (status != ZX_OK) {
      NXPF_ERR("Could not enable WEP key: %s", zx_status_get_string(status));
      return status;
    }
  }

  // We need to clear the IEs first to reset the internal MLAN state. This is especially true in
  // case there are no security IEs, otherwise the previous security settings might be applied to
  // the new connection.
  status = ClearIes();
  if (status != ZX_OK) {
    NXPF_ERR("Failed to clear IEs: %s", zx_status_get_string(status));
    return status;
  }

  uint8_t pairwise_cipher_suite = 0;
  uint8_t group_cipher_suite = 0;

  if (!req->has_security_ie() || req->security_ie().count() <= 0) {
    NXPF_INFO("No security_ie found in connect request.");
  } else {
    status = ConfigureIes(req->security_ie().data(), req->security_ie().count());
    if (status != ZX_OK) {
      NXPF_ERR("Failed to configure security IEs: %s", zx_status_get_string(status));
      return status;
    }

    status = GetRsnCipherSuites(req->security_ie().data(), req->security_ie().count(),
                                &pairwise_cipher_suite, &group_cipher_suite);
    // ZX_ERR_NOT_FOUND indicates there was no RSN IE, this could happen for an open network for
    // example. Don't treat this as an error, just use the default values of 0, there doesn't seem
    // to be a useful constant for this.
    if (status != ZX_OK && status != ZX_ERR_NOT_FOUND) {
      NXPF_ERR("Failed to get cipher suite from IEs: %s", zx_status_get_string(status));
      return status;
    }
  }

  status = SetAuthMode(req->auth_type());
  if (status != ZX_OK) {
    NXPF_ERR("Failed to set auth mode %u: %s", req->auth_type(), zx_status_get_string(status));
    return status;
  }

  status = SetEncryptMode(pairwise_cipher_suite);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to set pairwise cipher suite: %s", zx_status_get_string(status));
    return status;
  }
  if (pairwise_cipher_suite != group_cipher_suite) {
    // Only need to set the group cipher suite if it's different from the pairwise cipher suite.
    status = SetEncryptMode(group_cipher_suite);
    if (status != ZX_OK) {
      NXPF_ERR("Failed to set group cipher suite: %s", zx_status_get_string(status));
      return status;
    }
  }

  if (is_wpa_cipher_suite(pairwise_cipher_suite) || is_wpa_cipher_suite(group_cipher_suite)) {
    status = SetWpaEnabled(true);
    if (status != ZX_OK) {
      NXPF_ERR("Failed to enable WPA: %s", zx_status_get_string(status));
      return status;
    }
  }

  on_connect_ = std::move(on_connect);

  const bool is_host_mlme = req->auth_type() == fuchsia_wlan_fullmac::wire::WlanAuthType::kSae;
  auto on_connect_complete = [this, is_host_mlme](mlan_ioctl_req* req, IoctlStatus io_status) {
    std::lock_guard lock(mutex_);
    if (state_ != State::Connecting) {
      NXPF_WARN("Connection ioctl completed when no connection was in progress");
      return;
    }

    if (io_status == IoctlStatus::Timeout) {
      NXPF_WARN("Connection attempt timed out");
      CompleteConnection(StatusCode::kRefusedReasonUnspecified);
      return;
    }
    if (io_status == IoctlStatus::Canceled) {
      CompleteConnection(StatusCode::kCanceled);
      return;
    }

    auto& request = reinterpret_cast<IoctlRequest<mlan_ds_bss>*>(req)->UserReq();
    const mlan_ds_misc_assoc_rsp& assoc_rsp = request.param.ssid_bssid.assoc_rsp;

    const uint8_t* ies = nullptr;
    size_t ies_size = 0;

    StatusCode status_code = StatusCode::kSuccess;
    if (is_host_mlme) {
      if (is_association_reponse(assoc_rsp.assoc_resp_buf, assoc_rsp.assoc_resp_len)) {
        ies = assoc_rsp.assoc_resp_buf + sizeof(wlan::MgmtFrameHeader) +
              sizeof(wlan::AssociationResponse);
        ies_size = assoc_rsp.assoc_resp_buf + assoc_rsp.assoc_resp_len - ies;
        auto response = reinterpret_cast<const wlan::AssociationResponse*>(
            assoc_rsp.assoc_resp_buf + sizeof(wlan::MgmtFrameHeader));
        status_code = static_cast<StatusCode>(response->status_code);
      }
    } else {
      if (assoc_rsp.assoc_resp_len >= sizeof(IEEEtypes_AssocRsp_t)) {
        auto response = reinterpret_cast<const IEEEtypes_AssocRsp_t*>(assoc_rsp.assoc_resp_buf);
        ies = response->ie_buffer;
        ies_size = assoc_rsp.assoc_resp_len - sizeof(IEEEtypes_AssocRsp_t) + 1;
        status_code = static_cast<StatusCode>(response->status_code);
      }
    }

    if (req->status_code == MLAN_ERROR_NO_ERROR && io_status == IoctlStatus::Success) {
      if (status_code != StatusCode::kSuccess) {
        NXPF_WARN("Unexpectedly status code is %u", status_code);
        status_code = StatusCode::kSuccess;
      }
    } else if (status_code == StatusCode::kSuccess) {
      // The connection failed but there was no association response to get a status code from. Make
      // sure we indicate failure here.
      status_code = StatusCode::kJoinFailure;
    }

    CompleteConnection(status_code, ies, ies_size);
  };

  *connect_request_ = IoctlRequest<mlan_ds_bss>(
      MLAN_IOCTL_BSS, MLAN_ACT_SET, bss_index_,
      mlan_ds_bss{
          .sub_command = MLAN_OID_BSS_START,
          .param = {.ssid_bssid = {.idx = bss_index_,
                                   .channel = req->selected_bss().channel.primary,
                                   .host_mlme = is_host_mlme}},
      });
  mlan_ssid_bssid& bss = connect_request_->UserReq().param.ssid_bssid;
  memcpy(bss.bssid, req->selected_bss().bssid.data(), ETH_ALEN);
  bss.ssid.ssid_len = ssid->size();
  memcpy(bss.ssid.ssid, ssid->data(), bss.ssid.ssid_len);

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctl(
      connect_request_.get(), std::move(on_connect_complete), kConnectionTimeout);
  if (io_status != IoctlStatus::Pending) {
    // Even IoctlStatus::Success should  be considered a failure here. Connecting has to be a
    // pending operation, anything else is unreasonable.
    NXPF_ERR("Connect ioctl failed: %d", io_status);
    return ZX_ERR_IO;
  }

  // The connection attempt is now in progress.
  state_ = State::Connecting;

  return ZX_OK;
}

zx_status_t ClientConnection::CancelConnect() {
  std::lock_guard lock(mutex_);

  if (state_ != State::Connecting) {
    // No connection in progress
    return ZX_ERR_NOT_FOUND;
  }

  context_->ioctl_adapter_->CancelIoctl(connect_request_.get());

  return ZX_OK;
}

zx_status_t ClientConnection::Disconnect(
    const uint8_t* addr, uint16_t reason_code,
    std::function<void(IoctlStatus)>&& on_disconnect_complete) {
  std::lock_guard lock(mutex_);
  if (state_ == State::Disconnecting) {
    return ZX_ERR_ALREADY_EXISTS;
  }
  if (state_ != State::Connected) {
    return ZX_ERR_NOT_CONNECTED;
  }

  State previous_state = state_.Load();
  state_ = State::Disconnecting;

  // Stop the log timer unconditionally.
  log_timer_->Stop();

  auto request = std::make_unique<IoctlRequest<mlan_ds_bss>>(
      MLAN_IOCTL_BSS, MLAN_ACT_SET, bss_index_,
      mlan_ds_bss{.sub_command = MLAN_OID_BSS_STOP,
                  .param = {.deauth_param{.reason_code = reason_code}}});
  memcpy(request->UserReq().param.deauth_param.mac_addr, addr, ETH_ALEN);

  auto on_ioctl = [this, previous_state, on_disconnect = std::move(on_disconnect_complete)](
                      pmlan_ioctl_req req, IoctlStatus status) {
    {
      std::lock_guard lock(mutex_);
      state_ = status == IoctlStatus::Success ? State::Idle : previous_state;
    }

    on_disconnect(status);
    delete reinterpret_cast<const IoctlRequest<mlan_ds_bss>*>(req);
  };

  const IoctlStatus io_status =
      context_->ioctl_adapter_->IssueIoctl(request.get(), std::move(on_ioctl), kDisconnectTimeout);
  if (io_status != IoctlStatus::Pending) {
    NXPF_ERR("Failed to disconnect: %d", io_status);
    state_ = previous_state;
    return ZX_ERR_INTERNAL;
  }

  // At this point the request is pending and the allocated memory will be handled by the callback.
  (void)request.release();

  return ZX_OK;
}

zx_status_t ClientConnection::TransmitAuthFrame(const uint8_t* source_mac,
                                                const uint8_t* destination_mac, uint16_t sequence,
                                                uint16_t status_code,
                                                cpp20::span<const uint8_t> sae_fields) {
  uint8_t channel = 0;
  {
    std::lock_guard lock(mutex_);
    if (state_ != State::Authenticating) {
      NXPF_ERR("Attempted to transmit auth frame when in wrong state %d", state_.Load());
      return ZX_ERR_BAD_STATE;
    }
    if (!connect_request_params_) {
      NXPF_ERR("Attempt to send auth frame without first initiating a connection attempt");
      return ZX_ERR_BAD_STATE;
    }
    channel = connect_request_params_->get()->selected_bss().channel.primary;
    client_mac_.Set(source_mac);
    ap_mac_.Set(destination_mac);
  }

  std::optional<wlan::drivers::components::Frame> frame = context_->data_plane_->AcquireFrame();
  if (!frame.has_value()) {
    NXPF_ERR("Failed to acquire frame container for SAE frame");
    return ZX_ERR_NO_MEMORY;
  }

  NXPF_INFO("Transmit SAE auth frame (%d of 4)", sequence == 1 ? 1 : 3);

  struct AuthFrame {
    uint32_t type;
    uint32_t tx_ctrl;
    uint16_t len;
    IEEE80211_MGMT mgmt;
  } __PACKED;

  // The length of the auth payload, this is the whole management frame header plus the auth message
  // and the SAE auth data.
  const size_t auth_length =
      offsetof(IEEE80211_MGMT, u) + sizeof(IEEEtypes_Auth_framebody) + sae_fields.size();

  // The packet length is the full auth length plus the type, tx_ctrl and len fields in the header.
  const size_t packet_length = auth_length + offsetof(AuthFrame, mgmt);

  if (packet_length >= std::numeric_limits<uint16_t>::max()) {
    NXPF_ERR("Invalid SAE frame length %zu", packet_length);
    return ZX_ERR_INVALID_ARGS;
  }
  if (auth_length >= std::numeric_limits<uint16_t>::max() || auth_length > packet_length) {
    NXPF_ERR("Invalid auth frame length %zu", auth_length);
    return ZX_ERR_INVALID_ARGS;
  }
  if (packet_length >= frame->Size()) {
    NXPF_ERR("SAE frame size %zu exceeds available frame space %u", packet_length, frame->Size());
    return ZX_ERR_INVALID_ARGS;
  }

  frame->ShrinkHead(256);
  frame->SetPortId(static_cast<uint8_t>(bss_index_));
  frame->SetSize(static_cast<uint32_t>(packet_length));
  frame->SetPriority(kSaeFramePriority);

  auto auth_frame = reinterpret_cast<AuthFrame*>(frame->Data());
  memset(auth_frame, 0, sizeof(*auth_frame));

  auth_frame->type = PKT_TYPE_MGMT_FRAME;
  auth_frame->tx_ctrl = 0;
  auth_frame->len = static_cast<uint16_t>(auth_length);

  // Frame Control
  auth_frame->mgmt.frame_control = wlan::kManagement | (wlan::kAuthentication << 4);

  memcpy(auth_frame->mgmt.da, destination_mac, ETH_ALEN);                  // Destination
  memcpy(auth_frame->mgmt.sa, source_mac, ETH_ALEN);                       // Source
  memcpy(auth_frame->mgmt.bssid, destination_mac, ETH_ALEN);               // BSSID
  memcpy(auth_frame->mgmt.addr4, wlan::common::kBcastMac.byte, ETH_ALEN);  // addr4

  auth_frame->mgmt.u.auth.auth_alg = wlan::kSae;
  auth_frame->mgmt.u.auth.auth_transaction = sequence;
  auth_frame->mgmt.u.auth.status_code = static_cast<uint16_t>(status_code);

  memcpy(auth_frame->mgmt.u.auth.variable, sae_fields.data(), sae_fields.size());

  // Before we transmit the SAE auth frame make sure that firmware will send it on the correct
  // channel. Do this by instructing firmware to remain on the given channel.
  zx_status_t status = RemainOnChannel(channel);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to remain on channel %u: %s", channel, zx_status_get_string(status));
    return status;
  }

  // Send this as a raw data buffer, it's not an ethernet frame.
  status = context_->data_plane_->SendFrame(std::move(*frame), MLAN_BUF_TYPE_RAW_DATA);
  if (status != ZX_OK) {
    NXPF_ERR("Failed to transmit SAE auth frame: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t ClientConnection::OnSaeResponse(const uint8_t* peer, uint16_t status_code) {
  std::lock_guard lock(mutex_);
  return OnSaeResponseLocked(peer, status_code);
}

zx_status_t ClientConnection::OnSaeResponseLocked(const uint8_t* peer, uint16_t status_code) {
  if (state_ != State::Authenticating) {
    if (peer) {
      // This was externally generated, warn about this unexpected state. Internal calls might make
      // this call just to clean up state, no need to log for those.
      NXPF_WARN("SAE handshake completion when not authenticating, ignoring.");
    }
    return ZX_ERR_NOT_FOUND;
  }

  // Deregister for management frames (equivalent to registering for an empty list).
  RegisterForMgmtFrames({});

  // Cancel remain on channel now that the SAE handshake has completed and the normal connection
  // process will ensure that the correct channel is used.
  CancelRemainOnChannel();

  sae_timeout_timer_.Stop();

  if (status_code != STATUS_CODE_SUCCESS) {
    NXPF_ERR("SAE handshake failed: %u", status_code);
    state_ = State::Idle;
    return ZX_ERR_INTERNAL;
  }

  std::unique_ptr<ConnectRequestParams> params(std::move(connect_request_params_));
  OnConnectCallback on_connect = std::move(on_connect_);
  zx_status_t status = ConnectLocked(params->get(), std::move(on_connect));
  if (status != ZX_OK) {
    NXPF_ERR("Failed to resume connection after SAE handshake: %s", zx_status_get_string(status));
    state_ = State::Idle;
    return status;
  }
  return ZX_OK;
}

void ClientConnection::OnDisconnect(uint16_t reason_code) {
  NXPF_INFO("Client disconnect, reason: %u", reason_code);
  std::lock_guard lock(mutex_);
  if (state_ == State::Disconnecting && reason_code == 0) {
    // If there is a disconnect in progress and the reason code is zero this indicates that this
    // disconnect event is the result of the disconnect call by the driver. Don't handle this case
    // here, it will be handled when the disconnect ioctl completes. The ioctl seems to complete
    // after this event so it should be the safe choice.
    NXPF_INFO("Driver initiated disconnect");
    return;
  }
  if (state_ == State::Connecting) {
    // Attempt to cancel any ongoing connection attempt, if the cancel succeeds the connect callback
    // will be called with an indication that the connection failed.
    if (!context_->ioctl_adapter_->CancelIoctl(connect_request_.get())) {
      // If we can't cancel the ioctl and connect_in_progress_ is still set it means that the ioctl
      // must have been completed but the ioctl callback has yet to run. It seems like mlan should
      // not allow this case to happen, log an error message so it can be caught if it does happen.
      NXPF_ERR("Could not cancel connection attempt during disconnect event");
    }
    return;
  }
  if (state_ == State::Authenticating) {
    OnSaeResponseLocked(nullptr, STATUS_CODE_SPURIOUS_DEAUTH_OR_DISASSOC);
    return;
  }
  if (state_ != State::Connected) {
    NXPF_ERR("Received disconnect event when not connected, reason: %u", reason_code);
    return;
  }
  state_ = State::Idle;
  // Stop the log timer since the client is disconnected.
  log_timer_->Stop();
  ifc_->OnDisconnectEvent(reason_code);
}

void ClientConnection::OnManagementFrame(pmlan_event event) {
  // mlan copies the original event ID into the first four bytes.
  constexpr size_t kMgmtHdrOffset = 4u;
  // mlan also adds an additional fourth MAC address, compensate for this here.
  constexpr size_t kPayloadOffset = kMgmtHdrOffset + sizeof(wlan::MgmtFrameHeader) + ETH_ALEN;
  if (event->event_len < kPayloadOffset) {
    NXPF_ERR("Invalid management frame, size %u is less than minimum size of %zu", event->event_len,
             kPayloadOffset);
    return;
  }

  std::lock_guard lock(mutex_);
  if (state_ != State::Authenticating) {
    NXPF_WARN("Received management frame when authentication not in progress, ignoring.");
    return;
  }

  auto mgmt_hdr = reinterpret_cast<const wlan::MgmtFrameHeader*>(event->event_buf + kMgmtHdrOffset);

  if (client_mac_ != mgmt_hdr->addr1) {
    NXPF_WARN("Received management frame for unexpected client during authentication, ignoring.");
    return;
  }
  if (ap_mac_ != mgmt_hdr->addr3) {
    NXPF_WARN("Received management frame from unexpected AP during authentication, ignoring.");
    return;
  }

  const uint8_t* payload = event->event_buf + kPayloadOffset;
  const size_t payload_len = event->event_len - kPayloadOffset;
  switch (mgmt_hdr->fc.subtype()) {
    case kDisassociation: {
      if (payload_len < sizeof(wlan::Disassociation)) {
        NXPF_ERR("Invalid disassociation frame size %u < %zu", payload_len,
                 sizeof(wlan::Disassociation));
        return;
      }
      auto disassoc = reinterpret_cast<const wlan::Disassociation*>(payload);
      NXPF_INFO("Disassociated during authentication, reason: %u", disassoc->reason_code);
      OnSaeResponseLocked(nullptr, STATUS_CODE_SPURIOUS_DEAUTH_OR_DISASSOC);
    } break;
    case kAuthentication: {
      if (payload_len < sizeof(wlan::Authentication)) {
        NXPF_ERR("Invalid auth frame size %u < %zu", payload_len, sizeof(wlan::Authentication));
        return;
      }
      auto auth = reinterpret_cast<const wlan::Authentication*>(payload);
      NXPF_INFO("Received SAE auth frame (%d of 4)", auth->auth_txn_seq_number == 1 ? 2 : 4);

      const uint8_t* trail = payload + sizeof(wlan::Authentication);
      const size_t trail_size = payload_len - sizeof(wlan::Authentication);
      ifc_->OnAuthFrame(mgmt_hdr->addr3.byte, auth, cpp20::span<const uint8_t>(trail, trail_size));
    } break;
    case kDeauthentication: {
      if (payload_len < sizeof(wlan::Deauthentication)) {
        NXPF_ERR("Invalid deauth frame size %u < %zu", payload_len, sizeof(wlan::Deauthentication));
        return;
      }
      auto deauth = reinterpret_cast<const wlan::Deauthentication*>(payload);
      NXPF_INFO("Deauthenticated during authentication, reason: %u", deauth->reason_code);
      OnSaeResponseLocked(nullptr, STATUS_CODE_SPURIOUS_DEAUTH_OR_DISASSOC);
    } break;
    default:
      NXPF_INFO("Unexpected management frame %u", mgmt_hdr->fc.subtype());
      break;
  }
}

void ClientConnection::OnSaeTimeout() {
  std::lock_guard lock(mutex_);
  OnSaeResponseLocked(nullptr, STATUS_CODE_JOIN_FAILURE);
  CompleteConnection(StatusCode::kJoinFailure);
}

zx_status_t ClientConnection::InitiateSaeHandshake(
    const fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest* req) {
  // The SAE handshake requires that we manually handle some management frames. Register to receive
  // them here before the authentication process starts.
  zx_status_t status = RegisterForMgmtFrames({wlan::ManagementSubtype::kAuthentication,
                                              wlan::ManagementSubtype::kDeauthentication,
                                              wlan::ManagementSubtype::kDisassociation});
  if (status != ZX_OK) {
    NXPF_ERR("Failed to register for mgmt frames: %s", zx_status_get_string(status));
    return status;
  }

  ifc_->InitiateSaeHandshake(req->selected_bss().bssid.data());
  return ZX_OK;
}

zx_status_t ClientConnection::RegisterForMgmtFrames(
    const std::vector<wlan::ManagementSubtype>& types) {
  uint32_t subtype_mask = 0;
  for (auto type : types) {
    subtype_mask |= 1 << type;
  }

  IoctlRequest<mlan_ds_misc_cfg> request(
      MLAN_IOCTL_MISC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_misc_cfg{.sub_command = MLAN_OID_MISC_RX_MGMT_IND,
                       .param{.mgmt_subtype_mask = subtype_mask}});

  const IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to register for management frames 0x%08x: %d", subtype_mask, io_status);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t ClientConnection::RemainOnChannel(uint8_t channel) {
  IoctlRequest<mlan_ds_radio_cfg> request(
      MLAN_IOCTL_RADIO_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_radio_cfg{.sub_command = MLAN_OID_REMAIN_CHAN_CFG});
  auto& remain = request.UserReq().param.remain_chan;

  remain.channel = channel;
  remain.bandcfg.chanBand = band_from_channel(channel);
  remain.remain_period = kRemainOnChannelPeriodMillis;

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to remain on channel %u: %d", channel, io_status);
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

zx_status_t ClientConnection::CancelRemainOnChannel() {
  IoctlRequest<mlan_ds_radio_cfg> request(
      MLAN_IOCTL_RADIO_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_radio_cfg{.sub_command = MLAN_OID_REMAIN_CHAN_CFG});
  auto& remain = request.UserReq().param.remain_chan;

  remain.remove = true;

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to cancel remain on channel: %d", io_status);
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

zx_status_t ClientConnection::GetRsnCipherSuites(const uint8_t* ies, size_t ies_count,
                                                 uint8_t* out_pairwise_cipher_suite,
                                                 uint8_t* out_group_cipher_suite) {
  const IeView ie_view(ies, ies_count);

  const IEEEtypes_Rsn_t* rsn = ie_view.get_as<IEEEtypes_Rsn_t>(RSN_IE);
  if (!rsn) {
    return ZX_ERR_NOT_FOUND;
  }

  if (rsn->pairwise_cipher.count != 1) {
    // Not equipped to deal with this at this point.
    NXPF_INFO("Too many cipher counts: %u", rsn->pairwise_cipher.count);
    return ZX_ERR_INVALID_ARGS;
  }
  *out_pairwise_cipher_suite = rsn->pairwise_cipher.list[0].type;
  *out_group_cipher_suite = rsn->group_cipher.type;
  return ZX_OK;
}

zx_status_t ClientConnection::ClearIes() {
  IoctlRequest<mlan_ds_misc_cfg> request(
      MLAN_IOCTL_MISC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_misc_cfg{.sub_command = MLAN_OID_MISC_GEN_IE,
                       .param{.gen_ie{.type = MLAN_IE_TYPE_GEN_IE}}});

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to clear IEs: %d", io_status);
    return ZX_ERR_IO;
  }
  return ZX_OK;
}

zx_status_t ClientConnection::ConfigureIes(const uint8_t* ies, size_t ies_count) {
  const IeView ie_view(ies, ies_count);

  for (auto& ie : ie_view) {
    if (ie.type() == MOBILITY_DOMAIN) {
      // Ignore this IE.
      continue;
    }
    if (ie.raw_size() > std::numeric_limits<uint8_t>::max()) {
      NXPF_ERR("IE %u of size %u exceeds maximum size %u", ie.type(), ie.raw_size(),
               std::numeric_limits<uint8_t>::max());
      continue;
    }
    if (ie.is_vendor_specific_oui_type(kOuiMicrosoft, kOuiTypeWmm)) {
      // Do not include WMM IEs, some APs will reject the association.
      continue;
    }

    IoctlRequest<mlan_ds_misc_cfg> request(
        MLAN_IOCTL_MISC_CFG, MLAN_ACT_SET, bss_index_,
        mlan_ds_misc_cfg{.sub_command = MLAN_OID_MISC_GEN_IE,
                         .param{.gen_ie{.type = MLAN_IE_TYPE_GEN_IE, .len = ie.raw_size()}}});
    memcpy(request.UserReq().param.gen_ie.ie_data, ie.raw_data(), ie.raw_size());

    IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
    if (io_status != IoctlStatus::Success) {
      NXPF_ERR("Failed to set IE %u: %d", ie.type(), io_status);
      continue;
    }
  }

  return ZX_OK;
}

zx_status_t ClientConnection::SetAuthMode(fuchsia_wlan_fullmac::wire::WlanAuthType auth_type) {
  uint32_t auth_mode = 0;
  switch (auth_type) {
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem:
      auth_mode = MLAN_AUTH_MODE_OPEN;
      break;
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kSharedKey:
      // When asked to use a shared key (which should only happen for WEP), we will direct the
      // firmware to use auto-detect, which will fall back on open WEP if shared WEP fails to
      // succeed. This was chosen to allow us to avoid implementing WEP auto-detection at higher
      // levels of the wlan stack.
      auth_mode = MLAN_AUTH_MODE_AUTO;
      break;
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kFastBssTransition:
      auth_mode = MLAN_AUTH_MODE_FT;
      break;
    case fuchsia_wlan_fullmac::wire::WlanAuthType::kSae:
      auth_mode = MLAN_AUTH_MODE_SAE;
      break;
    default:
      NXPF_ERR("Invalid auth type %u", auth_type);
      return ZX_ERR_INVALID_ARGS;
  }

  IoctlRequest<mlan_ds_sec_cfg> request(
      MLAN_IOCTL_SEC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_sec_cfg{.sub_command = MLAN_OID_SEC_CFG_AUTH_MODE, .param{.auth_mode = auth_mode}});

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to set auth mode: %d", io_status);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t ClientConnection::SetEncryptMode(uint8_t cipher_suite) {
  uint32_t encrypt_mode = 0;

  switch (cipher_suite) {
    case 0:
      encrypt_mode = MLAN_ENCRYPTION_MODE_NONE;
      break;
    case CIPHER_SUITE_TYPE_WEP_40:
      encrypt_mode = MLAN_ENCRYPTION_MODE_WEP40;
      break;
    case CIPHER_SUITE_TYPE_WEP_104:
      encrypt_mode = MLAN_ENCRYPTION_MODE_WEP104;
      break;
    case CIPHER_SUITE_TYPE_TKIP:
      encrypt_mode = MLAN_ENCRYPTION_MODE_TKIP;
      break;
    case CIPHER_SUITE_TYPE_CCMP_128:
      encrypt_mode = MLAN_ENCRYPTION_MODE_CCMP;
      break;
    case CIPHER_SUITE_TYPE_CCMP_256:
      encrypt_mode = MLAN_ENCRYPTION_MODE_CCMP_256;
      break;
    case CIPHER_SUITE_TYPE_GCMP_128:
      encrypt_mode = MLAN_ENCRYPTION_MODE_GCMP;
      break;
    case CIPHER_SUITE_TYPE_GCMP_256:
      encrypt_mode = MLAN_ENCRYPTION_MODE_GCMP_256;
      break;
    default:
      NXPF_ERR("Unsupported cipher suite: %u", cipher_suite);
      return ZX_ERR_INVALID_ARGS;
  }

  IoctlRequest<mlan_ds_sec_cfg> request(
      MLAN_IOCTL_SEC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_sec_cfg{.sub_command = MLAN_OID_SEC_CFG_ENCRYPT_MODE,
                      .param{.encrypt_mode = encrypt_mode}});

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to set encrypt mode: %d", io_status);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t ClientConnection::SetWpaEnabled(bool enabled) {
  IoctlRequest<mlan_ds_sec_cfg> request(
      MLAN_IOCTL_SEC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_sec_cfg{.sub_command = MLAN_OID_SEC_CFG_WPA_ENABLED, .param{.wpa_enabled = enabled}});

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to %s WPA: %d", enabled ? "enable" : "disable", io_status);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

void ClientConnection::TriggerConnectCallback(StatusCode status_code, const uint8_t* ies,
                                              size_t ies_size) {
  if (on_connect_) {
    on_connect_(status_code, ies, ies_size);
    // Clear out the callback after using it.
    on_connect_ = OnConnectCallback();
  }
}

void ClientConnection::CompleteConnection(StatusCode status_code, const uint8_t* ies,
                                          size_t ies_size) {
  if (state_ != State::Connecting && state_ != State::Authenticating) {
    NXPF_WARN("Received connection completion with no connection attempt in progress, ignoring.");
    return;
  }
  state_ = status_code == StatusCode::kSuccess ? State::Connected : State::Idle;

  TriggerConnectCallback(status_code, ies, ies_size);
  // Start periodic timer to update logs/stats every 30 seconds if the connection was successful.
  if (state_ == State::Connected) {
    log_timer_->StartPeriodic(kLogTimerTimeout);
  }
}

void ClientConnection::IndicateSignalQuality() {
  IoctlRequest<mlan_ds_get_info> signal_req(MLAN_IOCTL_GET_INFO, MLAN_ACT_GET, bss_index_,
                                            {.sub_command = MLAN_OID_GET_SIGNAL});
  auto& signal_info = signal_req.UserReq().param.signal;

  IoctlStatus io_status = context_->ioctl_adapter_->IssueIoctlSync(&signal_req);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Get signal info req failed: %d", io_status);
    return;
  }
  NXPF_INFO("Client connection rssi %d snr %d", signal_info.data_rssi_avg,
            signal_info.data_snr_avg);
  ifc_->SignalQualityIndication((int8_t)signal_info.data_rssi_avg,
                                (int8_t)signal_info.data_snr_avg);
}

}  // namespace wlan::nxpfmac
