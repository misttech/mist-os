// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_BREDR_CONNECTION_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_BREDR_CONNECTION_SERVER_H_

#include <fuchsia/bluetooth/bredr/cpp/fidl.h>

#include "src/connectivity/bluetooth/core/bt-host/fidl/server_base.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/weak_self.h"

namespace bthost {

// BrEdrConnectionServer relays packets and disconnections between the Connection FIDL protocol and
// a corresponding L2CAP channel.
class BrEdrConnectionServer : public ServerBase<fuchsia::bluetooth::Channel> {
 public:
  // Limit unacknowledged OnReceive() events to 10, which should be small enough to avoid filling
  // FIDL channel.
  static constexpr uint8_t kDefaultReceiveCredits = 10;

  // The number of inbound packets to queue in this class when there are 0 receive credits left.
  static constexpr size_t kDefaultReceiveQueueLimit = 2;

  // `channel` is the Channel that this Connection corresponds to. BrEdrConnection server will
  // activate and manage the lifetime of this chanel. `closed_callback` will be called when either
  // the Connection protocol or the L2CAP channel closes. Returns nullptr on failure (failure to
  // activate the Channel).
  static std::unique_ptr<BrEdrConnectionServer> Create(
      fidl::InterfaceRequest<fuchsia::bluetooth::Channel> request,
      bt::l2cap::Channel::WeakPtr channel, fit::callback<void()> closed_callback);

  ~BrEdrConnectionServer() override;

 private:
  enum class State {
    kActivating,  // Default state.
    kActivated,
    kDeactivating,
    kDeactivated,
  };

  BrEdrConnectionServer(fidl::InterfaceRequest<fuchsia::bluetooth::Channel> request,
                        bt::l2cap::Channel::WeakPtr channel, fit::callback<void()> closed_callback);

  // fuchsia::bluetooth::Channel overrides:
  void Send(std::vector<uint8_t> packet, SendCallback callback) override;
  void AckReceive() override;
  void WatchChannelParameters(WatchChannelParametersCallback callback) override;
  void handle_unknown_method(uint64_t ordinal, bool method_has_response) override;

  bool Activate();
  void Deactivate();

  void OnChannelDataReceived(bt::ByteBufferPtr rx_data);
  void OnChannelClosed();
  void OnProtocolClosed();
  void DeactivateAndRequestDestruction();
  void ServiceReceiveQueue();

  bt::l2cap::Channel::WeakPtr channel_;

  // The maximum number of inbound packets to queue when the FIDL protocol is full.
  const size_t receive_queue_max_frames_ = kDefaultReceiveQueueLimit;

  // We use a std::deque here to minimize the number dynamic memory allocations (cf. std::list,
  // which would require allocation on each SDU). This comes, however, at the cost of higher memory
  // usage when the number of SDUs is small. (libc++ uses a minimum of 4KB per deque.)
  std::deque<bt::ByteBufferPtr> receive_queue_;

  // Client callback called when either FIDL protocol closes or L2CAP channel closes.
  fit::callback<void()> closed_cb_;

  // The maximum number of unacknowledged packets to send to the FIDL client at a time.
  uint8_t receive_credits_ = kDefaultReceiveCredits;

  // Pending callback for a WatchChannelParameters call.
  std::optional<WatchChannelParametersCallback> pending_watch_channel_parameters_;

  State state_ = State::kActivating;

  WeakSelf<BrEdrConnectionServer> weak_self_;  // Keep last.
};

}  // namespace bthost

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_BREDR_CONNECTION_SERVER_H_
