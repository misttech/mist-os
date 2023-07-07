<!--
Copyright 2023 The Fuchsia Authors. All rights reserved.
Use of this source code is governed by a BSD-style license that can be
found in the LICENSE file.

DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform tool.

See //docs/reference/syscalls/README.md#documentation-generation for
regeneration instructions.
-->

# zx_vmar_map_iob

## Summary

Map the memory regions attached to an IOBuffer. This call has
similar semantics to `zx_vmar_map`, however, mapping subsets of regions
is not supported.

## Declaration
```c
#include <zircon/syscalls.h>

zx_status_t zx_vmar_map_iob(zx_handle_t handle,
                             zx_vm_option_t options,
                             size_t vmar_offset,
                             zx_handle_t iob_ep,
                             uint32_t region_index,
                             size_t region_len,
                             zx_vaddr_t* addr_out);
```

## Description

Maps the given IOBuffer region into the given virtual memory
address region. Mapping a region retains a reference to the mapped
region, which means closing the endpoint that mapped the region does not
remove the mapping added by this function.

 *options* is equivalent to the *options* parameter of `zx_vmar_map`

 *vmar_offset* is equivalent to the *vmar_offset* parameter of `zx_vmar_map`.

 *ep* is the endpoint containing the region to map as created in
 `zx_iob_create`.

 *region_index* is the index of the memory region to map.

 *region_offset* is equivalent to the *vmo_offset* parameter of `zx_vmar_map`.

 *region_len* is equivalent to the *len* parameter of `zx_vmar_map`. It
 must by non-zero and page-aligned

## Return value

`zx_vmar_map_iob()` returns **ZX_OK** and the absolute base address of the
mapping (via *addr_out*) on success.  The base address will be page-aligned
and non-zero.  In the event of failure, a negative error value is returned.

## Errors

**ZX_ERR_BAD_HANDLE**  *handle* or *iob_ep* is not a valid handle.

**ZX_ERR_WRONG_TYPE**  *handle* or *iob_ep* is not a VMAR or IOB handle, respectively.

**ZX_ERR_BAD_STATE**  *handle* refers to a destroyed VMAR.

**ZX_ERR_INVALID_ARGS** for any of the following:
 - *mapped_addr* or *options* is not valid.
 - *vmar_offset* is non-zero when none of **ZX_VM_SPECIFIC**, **ZX_VM_SPECIFIC_OVERWRITE** or
   **ZX_VM_OFFSET_IS_UPPER_LIMIT** is specified.
 - **ZX_VM_SPECIFIC_OVERWRITE** and **ZX_VM_MAP_RANGE** are both specified.
 - **ZX_VM_OFFSET_IS_UPPER_LIMIT** is specified together with either **ZX_VM_SPECIFIC**
   or **ZX_VM_SPECIFIC_OVERWRITE**.
 - *vmar_offset* and *len* describe an unsatisfiable allocation due to exceeding the region bounds.
 - *vmar_offset* or *region_offset* is not page-aligned.
 - *region_len* is 0 or not page-aligned.

**ZX_ERR_ALREADY_EXISTS**  **ZX_VM_SPECIFIC** has been specified without
**ZX_VM_SPECIFIC_OVERWRITE**, and the requested range overlaps with another mapping.

**ZX_ERR_NO_RESOURCES** If a spot could not be found in the VMAR to create the mapping.

**ZX_ERR_ACCESS_DENIED**  Insufficient privileges to make the requested mapping.

**ZX_ERR_NOT_SUPPORTED** If the region is resizable, discardable, or backed by a pager but
**ZX_VM_ALLOW_FAULTS** is not set.

**ZX_ERR_BUFFER_TOO_SMALL** The region is not resizable and the mapping extends past the end
of the region but **ZX_VM_ALLOW_FAULTS** is not set.

**ZX_ERR_NO_MEMORY**  Failure due to lack of memory.
There is no good way for userspace to handle this (unlikely) error.
In a future build this error will no longer occur.

**ZX_ERR_OUT_OF_RANGE** for any of the following:
  - `region_offset + ROUNDUP(len, PAGE_SIZE)` overflows.
  - *region_index* is not valid for the provided IOB


## Notes

### VMAR Operations

Mapped regions generally support VMAR operations, such as `zx_vmar_protect`,
`zx_vmar_op_range`, `zx_vmar_destroy`. A region's access discipline may modify
or restrict these operations as described in the discipline spec.

## See also

 - [`zx_vmar_map()`]
 - [`zx_iob_create()`]
