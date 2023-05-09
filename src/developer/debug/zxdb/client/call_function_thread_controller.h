// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_CALL_FUNCTION_THREAD_CONTROLLER_H_
#define SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_CALL_FUNCTION_THREAD_CONTROLLER_H_

#include <map>

#include "src/developer/debug/shared/register_info.h"
#include "src/developer/debug/zxdb/client/arch_info.h"
#include "src/developer/debug/zxdb/client/finish_thread_controller.h"
#include "src/developer/debug/zxdb/client/function_return_info.h"
#include "src/developer/debug/zxdb/client/thread_controller.h"
#include "src/developer/debug/zxdb/common/address_ranges.h"

namespace zxdb {

// This is the top level class for calling functions in the target program. It
// is structured slightly differently than other thread controllers. Namely,
// this class does not implement all of the base ThreadController class, and is
// itself a base class for ABI specific thread controllers that fully implement
// the ThreadController interface and share some common code in this class.
//
// This is different from other thread controllers in that it prefers to use
// inheritance rather than composition, primarily because of the shared code
// needs that these classes have, which is unique from other thread controllers.
class CallFunctionThreadController : public ThreadController {
 public:
  ~CallFunctionThreadController() override;

  // ThreadController implementation.
  ContinueOp GetContinueOp() override;
  StopOp OnThreadStop(debug_ipc::ExceptionType stop_type,
                      const std::vector<fxl::WeakPtr<Breakpoint>>& hit_breakpoints) override;

  const char* GetName() const override { return "CallFunction"; }

 protected:
  struct RegisterCollection {
    RegisterCollection() = default;
    RegisterCollection(const Err& err, debug::RegisterCategory cat,
                       std::vector<debug::RegisterValue> regs)
        : err(err), category(cat), registers(std::move(regs)) {}
    Err err;
    debug::RegisterCategory category;
    std::vector<debug::RegisterValue> registers;
  };

  CallFunctionThreadController(const AddressRanges& ranges, FunctionReturnCallback cb,
                               fit::deferred_callback on_done);

  // Finds |id| in |regs| and updates its value to |value|. Does not perform any
  // IPC. Returns false if |id| was not found in |regs|.
  static bool WriteRegister(std::vector<debug::RegisterValue>& regs, debug::RegisterID id,
                            uint64_t value);

  // Returns the value of |id| in |regs| if found, 0 otherwise.
  static uint64_t GetRegisterData(const std::vector<debug::RegisterValue>& regs,
                                  debug::RegisterID id);

  void SetRegisterCategory(debug::RegisterCategory category,
                           const std::vector<debug::RegisterValue>& regs) {
    if (category == debug::RegisterCategory::kGeneral) {
      general_registers_ = regs;
    }
    saved_register_state_[category] = regs;
  }

  // Sends |general_registers_| to the target. It's up to the ABI implementation
  // to ensure that it has filtered out any unwriteable registers and that the
  // the General set of registers has already been collected before calling
  // this.
  void WriteGeneralRegisters(fit::callback<void(const Err&)> cb);

  virtual Frame* PushStackFrame(uint64_t new_pc, uint64_t old_sp,
                                const std::vector<debug::RegisterValue>& regs) = 0;

  virtual void CollectAllRegisterCategories(Thread* thread,
                                            fit::callback<void(const Err& err)> cb) = 0;

  // The address range of the function we're calling.
  AddressRanges address_ranges_;

  // This finish controller will be responsible for getting through the
  // synthetic stack frame that the ABI thread controller creates.
  std::unique_ptr<FinishThreadController> finish_controller_;

  // This callback is instantiated from
  FunctionReturnCallback function_return_callback_;

  // This will be a copy of the general registers that existed at the time
  // of calling the function. It will start as an exact copy of that data and be
  // changed as the thread state is configured for the new function, then the
  // resulting set of registers will be written to the target to kick things
  // off.
  std::vector<debug::RegisterValue> general_registers_;

 private:
  using RegisterMap = std::map<debug::RegisterCategory, std::vector<debug::RegisterValue>>;

  void CleanupFunction(fit::callback<void(const Err&)> cb);

  // The complete collection of registers before any modifications by the ABI
  // specific controllers.
  RegisterMap saved_register_state_;

  fxl::WeakPtrFactory<CallFunctionThreadController> weak_factory_;
};

}  // namespace zxdb

#endif  // SRC_DEVELOPER_DEBUG_ZXDB_CLIENT_CALL_FUNCTION_THREAD_CONTROLLER_H_
