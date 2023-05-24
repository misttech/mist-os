<!--
Copyright 2023 The Fuchsia Authors. All rights reserved.
Use of this source code is governed by a BSD-style license that can be
found in the LICENSE file.

DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform tool.

See //docs/reference/syscalls/README.md#documentation-generation for
regeneration instructions.
-->

# zx_restricted_kick

## Summary

Kick a thread out of restricted mode.

## Declaration

```c
#include <zircon/syscalls-next.h>

zx_status_t zx_restricted_kick(zx_handle_t thread, uint32_t options);
```

## Description

Kicks a thread out of restricted mode if it is currently running in restricted
mode or saves a pending kick if it is not. If the target thread is running in
restricted mode, it will exit to normal mode through the entry point provided to
`zx_restricted_enter` with a reason code set to `ZX_RESTRICTED_REASON_KICK`.
Otherwise the next call to `zx_restricted_enter` will not enter restricted mode
and will instead dispatch to the provided entry point with reason
code `ZX_RESTRICTED_REASON_KICK`.

Multiple kicks on the same thread object are collapsed together. Thus if
multiple threads call `zx_restricted_kick` on the same target while it is
running or entering restricted mode, at least one but possibly multiple
`ZX_RESTRICTED_REASON_KICK` returns will be observed. The recommended way to use
this syscall is to first record a reason for kicking in a synchronized data
structure and then call `zx_restricted_kick`. The thread calling
`zx_restricted_enter` should consult this data structure whenever it observes
`ZX_RESTRICTED_REASON_KICK` and process any pending state before reentering
restricted mode.

*options* must be zero.

## Rights

**ZX_RIGHT_MANAGE_THREAD** is required on *thread*.

## Errors

**ZX_ERR_INVALID_ARGS** *options* is any value other than 0.
**ZX_ERR_WRONG_TYPE** *thread* is not a thread.
**ZX_ERR_ACCESS_DENIED** *thread* does not have ZX_RIGHT_MANAGE_THREAD.
**ZX_ERR_BAD_STATE** *thread* is dead.
