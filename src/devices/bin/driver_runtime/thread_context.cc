// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_runtime/thread_context.h"

#include <zircon/assert.h>

#include <vector>

#include "src/devices/bin/driver_runtime/dispatcher.h"
#include "src/devices/lib/log/log.h"

namespace {

struct Entry {
  const void* driver;
  driver_runtime::Dispatcher* dispatcher;
};

static thread_local std::vector<Entry> g_driver_call_stack;
static thread_local Entry g_default_testing_state = {nullptr, nullptr};
// The latest generation seen by this thread.
static thread_local uint32_t g_cached_irqs_generation = 0;
// The result of setting the role profile for the current thread.
// May be std::nullopt if no attempt has been made to set the role profile.
static thread_local std::optional<zx_status_t> g_role_profile_status;

}  // namespace

namespace thread_context {

void PushDriver(const void* driver, driver_runtime::Dispatcher* dispatcher) {
  // TODO(https://fxbug.dev/42169761): re-enable this once driver host v1 is deprecated.
  // ZX_DEBUG_ASSERT(IsDriverInCallStack(driver) == false);
  if (IsDriverInCallStack(driver)) {
    LOGF(TRACE, "ThreadContext: tried to push driver %p that was already in stack\n", driver);
  }
  g_driver_call_stack.push_back({driver, dispatcher});
}

void PopDriver() {
  ZX_ASSERT(!g_driver_call_stack.empty());
  g_driver_call_stack.pop_back();
}

const void* GetCurrentDriver() {
  return g_driver_call_stack.empty() ? g_default_testing_state.driver
                                     : g_driver_call_stack.back().driver;
}

driver_runtime::Dispatcher* GetCurrentDispatcher() {
  return g_driver_call_stack.empty() ? g_default_testing_state.dispatcher
                                     : g_driver_call_stack.back().dispatcher;
}

void SetDefaultTestingDispatcher(driver_runtime::Dispatcher* dispatcher) {
  g_default_testing_state.dispatcher = dispatcher;
  g_default_testing_state.driver = dispatcher ? dispatcher->owner() : nullptr;
}

bool IsDriverInCallStack(const void* driver) {
  for (int64_t i = g_driver_call_stack.size() - 1; i >= 0; i--) {
    if (g_driver_call_stack[i].driver == driver) {
      return true;
    }
  }
  return false;
}

bool IsCallStackEmpty() { return g_driver_call_stack.empty(); }

uint32_t GetIrqGenerationId() { return g_cached_irqs_generation; }

void SetIrqGenerationId(uint32_t id) { g_cached_irqs_generation = id; }

std::optional<zx_status_t> GetRoleProfileStatus() { return g_role_profile_status; }

void SetRoleProfileStatus(zx_status_t status) { g_role_profile_status = status; }

}  // namespace thread_context
