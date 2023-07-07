<!--
Copyright 2023 The Fuchsia Authors. All rights reserved.
Use of this source code is governed by a BSD-style license that can be
found in the LICENSE file.

DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform tool.

See //docs/reference/syscalls/README.md#documentation-generation for
regeneration instructions.
-->

# zx_iob_create

## Summary

Create an IOBuffer with a set of options.

## Declaration
```c
#include <zircon/syscalls.h>

zx_status_t zx_iob_create(uint64_t options,
                          const zx_iob_region_t* regions,
                          uint32_t region_count,
                          zx_handle_t* ep0_out,
                          zx_handle_t* ep1_out);
```

## Description

`zx_iob_create()` creates an IOBuffer, a memory object designed for
efficient point-to-point communication. An IOBuffer can be thought
of an abstraction over a shared buffer with (optionally kernel mediated)
reads and writes that maintain data integrity and enforce permissions.

An IOBuffer may have multiple regions, specified by *region_count*.
Each region may be set to support varying access patterns or permissions
configured by **regions*.

### Region Descriptions

The geometry and configuration of a region are specified by a `zx_iob_region_t`
region description structure. The base structure includes fields that are common
to all region types.

```C++
struct zx_iob_region_t {
  uint32_t type;
  uint32_t access;
  uint64_t size;
  zx_iob_discipline_t discipline;
  union {
    zx_iob_region_private_t private_region;
    uint8_t max_extension[4 * 8];
  };
};
```

*type* specifies the type of the region and memory object backing it.
The valid types are:
  - ZX_IOB_REGION_TYPE_PRIVATE: a region backed by a private memory
    uniquely owned by the IOB.

*access* specifies the access control modifiers for each endpoint. It
must be a combination of one or more of:

 - **ZX_IOB_EP0_CAN_MAP_READ** to grant endpoint 0 to ability to map the region as readable
 - **ZX_IOB_EP0_CAN_MAP_WRITE** to grant endpoint 0 to ability to map the region as writable
 - **ZX_IOB_EP1_CAN_MAP_READ** to grant endpoint 1 to ability to map the region as readable
 - **ZX_IOB_EP1_CAN_MAP_WRITE** to grant endpoint 1 to ability to map the region as writable

*size* is the requested size of the region in bytes. The size will be
  rounded up to the next system page size boundary, as reported by
  zx_system_get_page_size(). Use `zx_object_get_info` with topic
  `ZX_INFO_IOB_REGIONS` to determine the actual size of the region.

*discipline* specifies the memory access discipline to employ for
 kernel-mediated operations. The valid disciplines are:
 - ZX_IOB_DISCIPLINE_TYPE_NONE: a free form region with no kernel mediated operations.

### Region Types
#### ZX_IOB_REGION_TYPE_PRIVATE

Specifies a region backed by a private memory object uniquely owned by the IOB.
This memory object is only accessible through operations on, and mappings of,
the owning IOB.

```
struct zx_iob_region_private_t {
  uint64_t options;
};
```

*options* must be 0

### Discipline Types
#### ZX_IOB_DISCIPLINE_TYPE_NONE

 ```C++
 struct iob_discipline_t {
   uint64_t type; // Must be ZX_IOB_DISCIPLINE_TYPE_NONE
 };
 ```

Specifies a free form region with no kernel mediated operations.

## Return value

`zx_iob_create()` returns **ZX_OK** on success. In the event of failure,
a negative error value is returned.

## Errors

**ZX_ERR_INVALID_ARGS**  *ep_out0* or *ep_out1* is an invalid pointer or
NULL, *options* is any value other than 0, or the regions configuration
is invalid

**ZX_ERR_NO_MEMORY**  Failure due to lack of memory to allocated the
requested buffers.
