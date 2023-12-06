<!--
Copyright 2023 The Fuchsia Authors. All rights reserved.
Use of this source code is governed by a BSD-style license that can be
found in the LICENSE file.

DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform tool.

See //docs/reference/syscalls/README.md#documentation-generation for
regeneration instructions.
-->

# zx_system_suspend_enter

# Summary

TODO(fxbug.dev/137059): Document exact behavior and interface as it iterates.

## Declaration

```c
#include <zircon/syscalls-next.h>

zx_status_t zx_system_suspend_enter(zx_handle_t resource, zx_time_t resume_deadline);
```

## Description

`zx_system_suspend_enter` suspends task execution on all online processors until the
absolute time given by *resume_deadline*. Task execution is resumed on all online processors
and the call to `zx_system_suspend_enter` returns when the resume deadline expires. Offline
processor states are not affected.

## Return value

**ZX_OK** when *resume_deadline* expires and the system resumes.

## Errors

**ZX_ERR_TIMED_OUT** when *resume_deadline* is in the past.

**ZX_ERR_BAD_HANDLE** *resource* is not a valid handle.

**ZX_ERR_WRONG_TYPE** *resource* is not resource kind **ZX_RSRC_KIND_SYSTEM**.

**ZX_ERR_OUT_OF_RANGE** *resource* is not in the range [**ZX_RSRC_SYSTEM_CPU_BASE**,
**ZX_RSRC_SYSTEM_CPU_BASE**+1).
