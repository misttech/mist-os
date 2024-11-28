// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_RUNTIME_THREAD_CONTEXT_H_
#define SRC_DEVICES_BIN_DRIVER_RUNTIME_THREAD_CONTEXT_H_

#include <zircon/types.h>

#include <optional>

namespace driver_runtime {
class Dispatcher;
}  // namespace driver_runtime

namespace thread_context {

// Adds |driver| to the thread's current call stack.
void PushDriver(const void* driver, driver_runtime::Dispatcher* dispatcher = nullptr);

// Removes the driver at the top of the thread's current call stack.
// The stack must not be empty.
void PopDriver();

// Returns the driver at the top of the thread's current call stack,
// or null if the stack is empty.
const void* GetCurrentDriver();

// Returns the dispatcher at the top of the thread's current call stack,
// or null if the stack is empty.
driver_runtime::Dispatcher* GetCurrentDispatcher();

// Sets the default dispatcher to return in GetCurrentDispatcher
// when the driver context stack is empty. Only meant for testing.
void SetDefaultTestingDispatcher(driver_runtime::Dispatcher* dispatcher);

// Returns whether |driver| is in the thread's current call stack.
bool IsDriverInCallStack(const void* driver);

// Returns whether the thread's current call stack is empty.
bool IsCallStackEmpty();

// Returns the latest generation id seen by the current thread.
uint32_t GetIrqGenerationId();

// Sets the latest generation id seen by the current thread.
void SetIrqGenerationId(uint32_t id);

// Returns the result of setting the role profile for the current thread.
// May be std::nullopt if no attempt has been made to set the role profile.
std::optional<zx_status_t> GetRoleProfileStatus();

// Sets the result of setting the role profile for the current thread.
void SetRoleProfileStatus(zx_status_t status);

}  // namespace thread_context

#endif  // SRC_DEVICES_BIN_DRIVER_RUNTIME_THREAD_CONTEXT_H_
