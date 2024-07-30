// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_CONCURRENT_GLOBAL_CAPABILITY_H_
#define LIB_CONCURRENT_GLOBAL_CAPABILITY_H_

#include <zircon/compiler.h>

namespace concurrent {

// A global capability type to enable the use of Clang thread annotations to check certain classes
// of global states. The actual state may be global with respect to the program, thread, or
// processor, depending on how the capability is acquired and released.
//
// A constexpr capability instance may be defined in a header in global or preferably namespace
// scope to be shared among multiple translation units or it may be defined locally with a single
// translation unit.
//
// Example:
//
// #ifndef TRANSACTION_H_
// #define TRANSACTION_H_
//
// #include <lib/concurrent/global_capability.h>
//
// namespace acme {
//
// constexpr concurrent::GlobalCapability transaction_active;
//
// class Transaction {
//  public:
//   Transaction() __TA_ACQUIRE(transaction_active) {
//     ZX_DEBUG_ASSERT(Active() == nullptr);
//     SetActive(this);
//   }
//   ~Transaction() __TA_RELEASE(transaction_active) {
//     ZX_DEBUG_ASSERT(Active() == this);
//     SetActive(nullptr);
//    }
//
//    // Returns the active transaction or nullptr.
//    static Transaction* Active() {
//      return transaction_tl_;
//    }
//
//    // Operation that requires an active transaction. May be called from a scope where the
//    // Transaction instance is only conveniently accessible via Transaction::Active().
//    // Transitive dependencies on this operation must be annotated to require the same
//    // capability, aiding callers in determining that the API requires an active transaction
//    // as a pre-condition.
//    void Operation() __TA_REQUIRES(transaction_active);
//
//  private:
//    void SetActive(Transaction* transaction) {
//      transaction_tl_ = transaction;
//    }
//
//    inline static thread_local Transaction* transaction_tl_;
// };
//
// class UseTransaction {
//  public:
//    void DoSomething() __TA_REQUIRES(transaction_active) {
//      // Perform operations that depend on an active transaction ...
//      // Runtime assertion that Active() != nullptr is unnecessary.
//      Transaction::Active()->Operation();
//    }
// };
//
// }  // namespace acme
//
// #endif  // TRANSACTION_H_
//

struct __TA_CAPABILITY("capability") GlobalCapability {};

}  // namespace concurrent

#endif  // LIB_CONCURRENT_GLOBAL_CAPABILITY_H_
