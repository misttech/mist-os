// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_CREDIT_BASED_FLOW_CONTROL_TX_ENGINE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_CREDIT_BASED_FLOW_CONTROL_TX_ENGINE_H_

#include <deque>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/l2cap_defs.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/tx_engine.h"

namespace bt::l2cap::internal {

// The transmit half of a credit-based flow control mode connection-oriented
// channel. The "credit" in the credit-based flow control is essentially just
// a counter of how many PDUs can be sent to the peer. The peer will
// occasionally indicate that more credits are available, which allows more
// PDUs to be sent.
class CreditBasedFlowControlTxEngine final : public TxEngine {
 public:
  // Construct a credit-based flow control engine.
  //
  // |channel_id|          - The ID of the channel that packets should be sent
  //                         to.
  // |max_tx_sdu_size|     - Maximum size of an SDU (Tx MTU).
  // |channel|             - A |TxEngine::TxChannel| to send frames to.
  // |mode|                - Mode of credit-based flow control.
  // |max_tx_pdu_size|     - Maximum size of a PDU (Tx MPS).
  // |initial_credits|     - Initial value to use for credits. The transmit
  //                         engine can only send packets if it has sufficient
  //                         credits. See |AddCredits|.
  CreditBasedFlowControlTxEngine(ChannelId channel_id,
                                 uint16_t max_tx_sdu_size,
                                 TxChannel& channel,
                                 CreditBasedFlowControlMode mode,
                                 uint16_t max_tx_pdu_size,
                                 uint16_t initial_credits);
  ~CreditBasedFlowControlTxEngine() override;

  // Notify the engine that an SDU is ready for processing. See |TxEngine|.
  void NotifySduQueued() override;

  // Attempt to add credits to the transmit engine. The engine will only
  // transmit pending PDUs if it has sufficient credits to do so. If there
  // are pending PDUs, this function may invoke |SendFrame|.
  //
  // Each enqueued PDU requires a credit to send, and credits can only be
  // replenished through this function.
  //
  // Returns true if successful, or false if the credits value would cause the
  // number of credits to exceed 65535, and does not modify the credit value.
  bool AddCredits(uint16_t credits);

  // Provide the current count of unspent credits.
  uint16_t credits() const;
  // Provide the current count of queued segments. This should never exceed
  // |max_tx_sdu_size|/|max_tx_pdu_size|.
  size_t segments_count() const;

 private:
  // Segments a single SDU into one or more PDUs, enqueued in |segments_|
  void SegmentSdu(ByteBufferPtr sdu);
  // Send as many segments as possible (up to the total number of |segments_|),
  // keeping all invariants intact such as the queue of segments remaining and
  // the credit count.
  void TrySendSegments();
  // Dequeue SDUs, segment them, and send until credits are depleted or no more
  // SDUs are available in the queue.
  void ProcessSdus();

  CreditBasedFlowControlMode mode_;
  uint16_t max_tx_pdu_size_;
  uint16_t credits_;

  // An ordered collection of PDUs that make up the segments of the current
  // SDU being sent. May be partial if previous PDUs from the same SDU have
  // been sent, but there are insufficient credits for the remainder.
  std::deque<std::unique_ptr<DynamicByteBuffer>> segments_;
};
}  // namespace bt::l2cap::internal

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_CREDIT_BASED_FLOW_CONTROL_TX_ENGINE_H_
