<!--
Copyright 2023 The Fuchsia Authors. All rights reserved.
Use of this source code is governed by a BSD-style license that can be
found in the LICENSE file.

DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform tool.

See //docs/reference/syscalls/README.md#documentation-generation for
regeneration instructions.
-->

# zx_restricted_enter

## Summary

Enter restricted mode

## Declaration

```c
#include <zircon/syscalls-next.h>

zx_status_t zx_restricted_enter(uint32_t options,
                                uintptr_t vector_table_ptr,
                                uintptr_t context);
```

## Description

Enters restricted mode from normal thread state. If successful, the current
thread will return to normal mode via an entry point passed in
*vector_table_ptr*.

*vector_table_ptr* must be within the current user address space.
*context* may be any value. It is used as a value to pass back to normal
mode when returning from restricted mode.

*options* is a bit vector that contains zero more of the following:

- **ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL** indicates that any exceptions
  encountered while in restricted mode should be delivered using exception
  channels. If this option is not present then any exceptions not handled by a
  process debugger will cause control to vector to `vector_table_ptr` in normal
  mode with the *reason code* set to `ZX_RESTRICTED_REASON_EXCEPTION`.

Arguments to the function at *vector_table_ptr* are architecturally specific:

On x64, *context* is placed in *rdi* and a reason code is placed in *rsi*.
All other registers are currently undefined, including the stack pointer.

On arm64, *context* is placed in *x0* and a reason code is placed in *x1*.
All other registers are currently undefined, including the stack pointer.

On riscv64, *context* is placed in *a0* and a reason code is placed in *a1*.
All other registers are currently undefined, including the stack pointer.

The *reason code* specifies the reason that normal mode execution has resumed.
This *reason code* may be one of `ZX_RESTRICTED_REASON_SYSCALL`,
`ZX_RESTRICTED_REASON_EXCEPTION`.

### Shared process

Processes created with the `ZX_PROCESS_SHARED` option, or via `zx_process_create_shared()`
have two distinct [address spaces]. One is shared between multiple processes, while the other
is restricted to the specific process. When a thread that is entering restrcited mode
belongs to such a process, the active address space for the thread is updated as follows:

  - When entering restricted mode the active address space for the thread is set to the
    restricted address space of the process.
  - When exiting restricted mode the active address space for the thread is set to the
    shared address space of the process.

## Rights

None (currently)

## Return value

No return value on success, since the current thread indirectly returns via
*vector_table_ptr*. In the event of failure, a negative error value is returned.

## Errors

**ZX_ERR_INVALID_ARGS** *vector_table_ptr* is not a valid user address or *options*
is non-zero.

**ZX_ERR_BAD_STATE** restricted mode register state is invalid.

**ZX_ERR_NOT_SUPPORTED** `ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL` is _not_ provided and
vectored exceptions are not implemented for the current architecture.

## See also

- [`zx_restricted_bind_state()`]
- [`zx_restricted_unbind_state()`]
- [`zx_process_create_shared()`]

[`zx_restricted_bind_state()`]: restricted_bind_state.md
[`zx_restricted_unbind_state()`]: restricted_unbind_state.md
[`zx_process_create_shared()`]: process_create_shared.md
[address spaces]: /docs/concepts/memory/address_spaces.md
