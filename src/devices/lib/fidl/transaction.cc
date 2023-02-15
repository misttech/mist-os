// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/lib/fidl/transaction.h"

namespace {

// Reply originating from driver.
zx_status_t DdkReply(device_fidl_txn_t* txn, const fidl_outgoing_msg_t* msg) {
  auto message = fidl::OutgoingMessage::FromEncodedCMessage(msg);
  // If FromDdkInternalTransaction returns a unique_ptr variant, it will be destroyed when exiting
  // this scope.
  auto fidl_txn = FromDdkInternalTransaction(ddk::internal::Transaction::FromTxn(txn));
  std::visit([&](auto&& arg) { arg->Reply(&message); }, fidl_txn);
  return ZX_OK;
}

// Bitmask for checking if a pointer stashed in a ddk::internal::Transaction is from the heap or
// not. This is safe to use on our pointers, because fidl::Transactions are always aligned to more
// than one byte.
static_assert(alignof(fidl::Transaction) > 1);
constexpr uintptr_t kTransactionIsBoxed = 0x1;

}  // namespace

ddk::internal::Transaction MakeDdkInternalTransaction(fidl::Transaction* txn) {
  device_fidl_txn_t fidl_txn = {
      .reply = DdkReply,
      .driver_host_context = reinterpret_cast<uintptr_t>(txn),
  };
  return ddk::internal::Transaction(fidl_txn);
}

ddk::internal::Transaction MakeDdkInternalTransaction(std::unique_ptr<fidl::Transaction> txn) {
  device_fidl_txn_t fidl_txn = {
      .reply = DdkReply,
      .driver_host_context = reinterpret_cast<uintptr_t>(txn.release()) | kTransactionIsBoxed,
  };
  return ddk::internal::Transaction(fidl_txn);
}

std::variant<fidl::Transaction*, std::unique_ptr<fidl::Transaction>> FromDdkInternalTransaction(
    ddk::internal::Transaction* txn) {
  uintptr_t raw = txn->DriverHostCtx();
  ZX_ASSERT_MSG(raw != 0, "Reused adevice_fidl_txn_t!\n");

  // Invalidate the source transaction
  txn->Txn()->driver_host_context = 0;

  auto ptr = reinterpret_cast<fidl::Transaction*>(raw & ~kTransactionIsBoxed);
  if (raw & kTransactionIsBoxed) {
    return std::unique_ptr<fidl::Transaction>(ptr);
  }
  return ptr;
}
