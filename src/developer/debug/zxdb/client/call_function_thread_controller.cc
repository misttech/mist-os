// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/client/call_function_thread_controller.h"

#include <map>
#include <utility>
#include <vector>

#include "src/developer/debug/shared/logging/logging.h"
#include "src/developer/debug/shared/register_info.h"
#include "src/developer/debug/shared/register_value.h"
#include "src/developer/debug/zxdb/client/call_function_thread_controller_arm64.h"
#include "src/developer/debug/zxdb/client/call_function_thread_controller_x64.h"
#include "src/developer/debug/zxdb/client/frame.h"
#include "src/developer/debug/zxdb/client/process.h"
#include "src/developer/debug/zxdb/client/remote_api.h"
#include "src/developer/debug/zxdb/client/session.h"
#include "src/developer/debug/zxdb/client/step_mode.h"
#include "src/developer/debug/zxdb/client/step_thread_controller.h"
#include "src/developer/debug/zxdb/client/thread.h"
#include "src/developer/debug/zxdb/client/thread_controller.h"
#include "src/performance/trace/options.h"

namespace zxdb {

CallFunctionThreadController::CallFunctionThreadController(const AddressRanges& ranges,
                                                           FunctionReturnCallback cb,
                                                           fit::deferred_callback on_done)
    : ThreadController(std::move(on_done)),
      address_ranges_(ranges),
      function_return_callback_(std::move(cb)),
      weak_factory_(this) {}

CallFunctionThreadController::~CallFunctionThreadController() = default;

// static.
bool CallFunctionThreadController::WriteRegister(std::vector<debug::RegisterValue>& regs,
                                                 debug::RegisterID id, uint64_t value) {
  auto reg_it = std::find_if(regs.begin(), regs.end(),
                             [id](const debug::RegisterValue& rv) { return rv.id == id; });
  if (reg_it == regs.end())
    return false;

  *reg_it = debug::RegisterValue(id, value);
  return true;
}

// static.
uint64_t CallFunctionThreadController::GetRegisterData(
    const std::vector<debug::RegisterValue>& regs, debug::RegisterID id) {
  auto reg_it = std::find_if(regs.begin(), regs.end(),
                             [id](const debug::RegisterValue& rv) { return rv.id == id; });
  if (reg_it == regs.end())
    return 0;

  return reg_it->GetValue();
}

void CallFunctionThreadController::WriteGeneralRegisters(fit::callback<void(const Err&)> cb) {
  debug_ipc::WriteRegistersRequest request;
  request.id = {thread()->GetProcess()->GetKoid(), thread()->GetKoid()};
  request.registers = general_registers_;

  thread()->GetProcess()->session()->remote_api()->WriteRegisters(
      request,
      [weak_this = weak_factory_.GetWeakPtr(), weak_thread = thread()->GetWeakPtr(),
       cb = std::move(cb)](const Err& err, const debug_ipc::WriteRegistersReply& reply) mutable {
        if (err.has_error()) {
          return cb(err);
        } else if (reply.status.has_error()) {
          return cb(Err(reply.status.message()));
        } else if (!weak_thread) {
          return cb(Err("Thread disappeared."));
        } else if (!weak_this) {
          return cb(Err("CallFunctionThreadController disappeared."));
        }

        weak_this->Log("Spawning finish thread controller to complete synthetic frame.");

        auto& stack = weak_thread->GetStack();
        weak_this->finish_controller_ = std::make_unique<FinishThreadController>(
            stack, *stack.IndexForFrame(stack[0]),
            [weak_this](const FunctionReturnInfo& return_info) mutable {
              if (weak_this) {
                weak_this->function_return_callback_(return_info);
              }
            });

        weak_this->finish_controller_->InitWithThread(weak_thread.get(), std::move(cb));
      });
}

void CallFunctionThreadController::CleanupFunction(fit::callback<void(const Err&)> cb) {
  Log("CallFunctionThreadController::CleanupFunction");

  debug_ipc::WriteRegistersRequest request;
  request.id = {.process = thread()->GetProcess()->GetKoid(), .thread = thread()->GetKoid()};

  for (auto [cat, regs] : saved_register_state_) {
    request.registers.insert(request.registers.end(), regs.begin(), regs.end());
  }

  thread()->session()->remote_api()->WriteRegisters(
      request, [weak_this = weak_factory_.GetWeakPtr(), cb = std::move(cb)](
                   const Err& err, const debug_ipc::WriteRegistersReply& reply) mutable {
        if (!weak_this) {
          return cb(Err("CallFunctionThreadController disappeared."));
        } else if (err.has_error()) {
          weak_this->saved_register_state_.clear();
          return cb(Err("Got error while restoring registers: " + err.msg()));
        } else if (reply.status.has_error()) {
          weak_this->saved_register_state_.clear();
          return cb(Err("Got WriteRegistersReply status error: " + reply.status.message()));
        }

        // Force sync the registers to the top most frame, which should be the
        // same one we started from. This ensures that if the user types "regs"
        // after calling the function they should be identical to what they were
        // before. Once this completes, we're done.
        weak_this->thread()->GetStack()[0]->GetRegisterCategoryAsync(
            debug::RegisterCategory::kGeneral, true,
            [weak_this, cb = std::move(cb)](const Err& err,
                                            const std::vector<debug::RegisterValue>& regs) mutable {
              if (weak_this) {
                // Signal to |OnThreadStop| that the work is finished.
                weak_this->saved_register_state_.clear();
              }

              if (cb)
                return cb(err);
            });
      });
}

ThreadController::ContinueOp CallFunctionThreadController::GetContinueOp() {
  return finish_controller_->GetContinueOp();
}

ThreadController::StopOp CallFunctionThreadController::OnThreadStop(
    debug_ipc::ExceptionType stop_type,
    const std::vector<fxl::WeakPtr<Breakpoint>>& hit_breakpoints) {
  Log("CallFunctionThreadController::OnThreadStop");

  if (finish_controller_) {
    auto op = finish_controller_->OnThreadStop(stop_type, hit_breakpoints);

    if (op == kStopDone) {
      // Okay the finish controller is done, now we need to cleanup the
      // register state which will happen asynchronously.
      finish_controller_.reset();

      CleanupFunction([weak_this = weak_factory_.GetWeakPtr(), stop_type](const Err& err) {
        if (weak_this && weak_this->thread()) {
          weak_this->thread()->ResumeFromAsyncThreadController(stop_type);
        }
      });

      return kFuture;
    }

    // The finish controller is not finished yet forward that back up to the
    // thread.
    return op;
  }

  // CleanupFunction will restore the register state from |all_registers_| to
  // the target and then clear it once the target confirms the register write.
  return saved_register_state_.empty() ? kStopDone : kFuture;
}

}  // namespace zxdb
