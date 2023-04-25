// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_HANDLER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_HANDLER_H_

#include <functional>

#define DEF_DEFAULT_HANDLER_BEGIN(opcode_type, handler_func_type) \
  void SetHook(opcode_type opcode, handler_func_type func) {      \
    handlers_[opcode] = std::move(func);                          \
  }                                                               \
  std::unordered_map<opcode_type, handler_func_type> handlers_ = {
#define DEF_DEFAULT_HANDLER_END() \
  }                               \
  ;
#define DEF_DEFAULT_HANDLER(opcode, default_func_name) {opcode, default_func_name},

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_HANDLER_H_
