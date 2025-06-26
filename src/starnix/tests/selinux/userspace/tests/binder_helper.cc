// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/selinux/userspace/tests/binder_helper.h"

namespace binder_helper {

ParsedMessage ParseMessage(const binder_uintptr_t start, const binder_size_t length) {
  // This function is based on the code of `printReturnCommand`:
  // https://cs.android.com/android/platform/superproject/+/master:frameworks/native/libs/binder/IPCThreadState.cpp;drc=bf14463e0c2309f04d0ba25cf951dcea3c47858e;l=153

  ParsedMessage m;

  const binder_uintptr_t end = start + length;

  binder_uintptr_t ptr = start;
  while (ptr < end) {
    binder_driver_return_protocol cmd = (binder_driver_return_protocol)(*(uint32_t *)ptr);
    m.commands_.push_back(cmd);
    ptr += sizeof(uint32_t);
    switch (cmd) {
      case BR_TRANSACTION_SEC_CTX:
        ptr += sizeof(binder_transaction_data_secctx);
        break;
      case BR_TRANSACTION:
      case BR_REPLY:
        ptr += sizeof(binder_transaction_data);
        break;
      case BR_ACQUIRE_RESULT:
        ptr += sizeof(uint32_t);
        break;
      case BR_INCREFS:
      case BR_ACQUIRE:
      case BR_RELEASE:
      case BR_DECREFS:
        ptr += sizeof(uint32_t) * 2;
        break;
      case BR_ATTEMPT_ACQUIRE:
        ptr += sizeof(uint32_t) * 3;
        break;
      case BR_DEAD_BINDER:
      case BR_CLEAR_DEATH_NOTIFICATION_DONE:
        ptr += sizeof(uint32_t);
        break;
      case BR_OK:
      case BR_DEAD_REPLY:
      case BR_TRANSACTION_COMPLETE:
      case BR_FINISHED:
      case BR_NOOP:
      case BR_FAILED_REPLY:
      case BR_ERROR:
      default:
        break;
    }
  }
  return m;
}

}  // namespace binder_helper
