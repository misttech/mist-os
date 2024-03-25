// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_FAKE_TX_CHANNEL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_FAKE_TX_CHANNEL_H_

#include <lib/fit/function.h>

#include <queue>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/tx_engine.h"

namespace bt::l2cap::internal {

// A fake TxEngine::TxChannel, useful for testing.
class FakeTxChannel : public TxEngine::TxChannel {
 public:
  using SendFrameHandler = fit::function<void(ByteBufferPtr)>;

  ~FakeTxChannel() override = default;

  FakeTxChannel& HandleSendFrame(SendFrameHandler handler) {
    send_frame_cb_ = std::move(handler);
    return *this;
  }

  FakeTxChannel& QueueSdu(ByteBufferPtr sdu) {
    queue_.push(std::move(sdu));
    return *this;
  }

  void SendFrame(ByteBufferPtr frame) override {
    if (send_frame_cb_)
      send_frame_cb_(std::move(frame));
  }

  std::optional<ByteBufferPtr> GetNextQueuedSdu() override {
    if (queue_.empty())
      return std::nullopt;
    ByteBufferPtr next = std::move(queue_.front());
    queue_.pop();
    return next;
  }

  size_t queue_size() { return queue_.size(); }

 private:
  SendFrameHandler send_frame_cb_;
  std::queue<ByteBufferPtr> queue_;
};

}  // namespace bt::l2cap::internal

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_FAKE_TX_CHANNEL_H_
