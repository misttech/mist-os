// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/syscalls/forward.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <cstdint>

#include <ktl/utility.h>
#include <object/counter_dispatcher.h>
#include <object/handle.h>
#include <object/process_dispatcher.h>

// zx_status_t zx_counter_create
zx_status_t sys_counter_create(uint32_t options, zx_handle_t* handle_out) {
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();

  // TODO(https://fxbug.dev/387324141): Add/enforce ZX_POL_NEW_COUNTER policy.

  KernelHandle<CounterDispatcher> handle;
  zx_rights_t rights;
  zx_status_t result = CounterDispatcher::Create(&handle, &rights);
  if (result != ZX_OK) {
    return result;
  }

  return up->MakeAndAddHandle(ktl::move(handle), rights, handle_out);
}

// zx_status_t zx_counter_add
zx_status_t sys_counter_add(zx_handle_t handle, int64_t value) {
  auto up = ProcessDispatcher::GetCurrent();

  // Both read and write rights are required for add because the resulting signal state and error
  // code can be used to determine the counter's value.
  constexpr zx_rights_t kRights = ZX_RIGHT_READ | ZX_RIGHT_WRITE;

  fbl::RefPtr<CounterDispatcher> counter;
  zx_status_t status = up->handle_table().GetDispatcherWithRights(*up, handle, kRights, &counter);
  if (status != ZX_OK) {
    return status;
  }

  return counter->Add(value);
}

// zx_status_t zx_counter_read
zx_status_t sys_counter_read(zx_handle_t handle, user_out_ptr<int64_t> value_out) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<CounterDispatcher> counter;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_READ, &counter);
  if (status != ZX_OK) {
    return status;
  }

  const int64_t value = counter->Value();

  return value_out.copy_to_user(value);
}

// zx_status_t zx_counter_write
zx_status_t sys_counter_write(zx_handle_t handle, int64_t value) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<CounterDispatcher> counter;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &counter);
  if (status != ZX_OK) {
    return status;
  }

  counter->SetValue(value);

  return ZX_OK;
}
