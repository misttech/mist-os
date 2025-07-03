// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_TESTS_SELINUX_USERSPACE_TESTS_BINDER_HELPER_H_
#define SRC_STARNIX_TESTS_SELINUX_USERSPACE_TESTS_BINDER_HELPER_H_

#include <sys/types.h>

#include <vector>

#include <linux/android/binder.h>

namespace binder_helper {

struct ParsedMessage {
  std::vector<binder_driver_return_protocol> commands_;
};

ParsedMessage ParseMessage(const binder_uintptr_t start, const binder_size_t length);

struct __attribute__((packed)) TransactionPayload {
  uint32_t command_code = 0;
  struct binder_transaction_data data = {};
};

struct __attribute__((packed)) EnterLooperPayload {
  const uint32_t command_code = BC_ENTER_LOOPER;
};

}  // namespace binder_helper

#endif  // SRC_STARNIX_TESTS_SELINUX_USERSPACE_TESTS_BINDER_HELPER_H_
