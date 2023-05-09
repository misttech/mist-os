// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_CALL_FUNCTION_THREAD_CONTROLLER_X64_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_CALL_FUNCTION_THREAD_CONTROLLER_X64_H_

#include "src/developer/debug/zxdb/client/call_function_thread_controller.h"

namespace zxdb {

class CallFunctionThreadControllerX64 : public CallFunctionThreadController {
 public:
  CallFunctionThreadControllerX64(const AddressRanges& range, FunctionReturnCallback cb,
                                  fit::deferred_callback on_done = {});

  ~CallFunctionThreadControllerX64() override;

  // ThreadController implementation.
  void InitWithThread(Thread* thread, fit::callback<void(const Err&)> cb) override;
  const char* GetName() const override { return "CallFunctionX64"; }

  void CollectAllRegisterCategories(Thread* thread, fit::callback<void(const Err& err)> cb) final;

 private:
  Frame* PushStackFrame(uint64_t new_pc, uint64_t old_sp,
                        const std::vector<debug::RegisterValue>& regs) final;

  fxl::WeakPtrFactory<CallFunctionThreadControllerX64> weak_factory_;
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_CALL_FUNCTION_THREAD_CONTROLLER_X64_H_
