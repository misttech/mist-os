// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_BASIC_MODE_TX_ENGINE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_BASIC_MODE_TX_ENGINE_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/tx_engine.h"

namespace bt::l2cap::internal {

// Implements the sender-side functionality of L2CAP Basic Mode. See Bluetooth
// Core Spec v5.0, Volume 3, Part A, Sec 2.4, "Modes of Operation".
//
// THREAD-SAFETY: This class may is _not_ thread-safe. In particular, the class
// assumes that some other party ensures that QueueSdu() is not invoked
// concurrently with the destructor.
class BasicModeTxEngine final : public TxEngine {
 public:
  BasicModeTxEngine(ChannelId channel_id,
                    uint16_t max_tx_sdu_size,
                    TxChannel& channel)
      : TxEngine(channel_id, max_tx_sdu_size, channel) {}
  ~BasicModeTxEngine() override = default;

  // Notify that an SDU is ready for transmitting. See |TxEngine|.
  void NotifySduQueued() override;

 private:
  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(BasicModeTxEngine);
};

}  // namespace bt::l2cap::internal

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_BASIC_MODE_TX_ENGINE_H_
