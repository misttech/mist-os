// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/basic_mode_tx_engine.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/assert.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"

namespace bt::l2cap::internal {

void BasicModeTxEngine::NotifySduQueued() {
  std::optional<ByteBufferPtr> sdu = channel().GetNextQueuedSdu();

  BT_ASSERT(sdu);
  BT_ASSERT(*sdu);

  if ((*sdu)->size() > max_tx_sdu_size()) {
    bt_log(INFO,
           "l2cap",
           "SDU size exceeds channel TxMTU (channel-id: %#.4x)",
           channel_id());
    return;
  }

  channel().SendFrame(std::move(*sdu));
}

}  // namespace bt::l2cap::internal
