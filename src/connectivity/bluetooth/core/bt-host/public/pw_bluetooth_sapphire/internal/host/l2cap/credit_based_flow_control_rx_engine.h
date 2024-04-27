// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_CREDIT_BASED_FLOW_CONTROL_RX_ENGINE_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_CREDIT_BASED_FLOW_CONTROL_RX_ENGINE_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/rx_engine.h"

namespace bt::l2cap::internal {

// Implements the receiver state and logic for an L2CAP channel operating in
// either Enhanced or LE Credit-Based Flow Control Mode.
class CreditBasedFlowControlRxEngine final : public RxEngine {
 public:
  // Callback to invoke on a failure condition. In actual operation the
  // callback must disconnect the channel to remain compliant with the spec.
  // See Core Spec Ver 5.4, Vol 3, Part A, Sec 3.4.3.
  using FailureCallback = fit::callback<void()>;

  explicit CreditBasedFlowControlRxEngine(FailureCallback failure_callback);
  ~CreditBasedFlowControlRxEngine() override = default;

  ByteBufferPtr ProcessPdu(PDU pdu) override;

 private:
  FailureCallback failure_callback_;

  MutableByteBufferPtr next_sdu_ = nullptr;
  size_t valid_bytes_ = 0;

  // Call the failure callback and reset.
  void OnFailure();

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(CreditBasedFlowControlRxEngine);
};

}  // namespace bt::l2cap::internal

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_L2CAP_CREDIT_BASED_FLOW_CONTROL_RX_ENGINE_H_
