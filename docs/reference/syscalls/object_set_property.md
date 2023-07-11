<!--
Copyright 2023 The Fuchsia Authors. All rights reserved.
Use of this source code is governed by a BSD-style license that can be
found in the LICENSE file.

DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform tool.

See //docs/reference/syscalls/README.md#documentation-generation for
regeneration instructions.
-->

# zx_object_set_property

## Summary

Set various properties of various kernel objects.

## Declaration

```c
#include <zircon/syscalls.h>

zx_status_t zx_object_set_property(zx_handle_t handle,
                                   uint32_t property,
                                   const void* value,
                                   size_t value_size);
```

## Description

`zx_object_set_property()` modifies the value of a kernel object's property.
Setting a property requires **ZX_RIGHT_SET_PROPERTY** rights on the handle.

See [`zx_object_get_property()`] for a full description.

## Rights

*handle* must have **ZX_RIGHT_SET_PROPERTY**, and must be of a supported **ZX_OBJ_TYPE_**
for the *property*, as documented in [`zx_object_get_property()`].

## See also

 - [`zx_object_get_property()`]

[`zx_object_get_property()`]: object_get_property.md
