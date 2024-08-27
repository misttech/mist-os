// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_USERBOOT_INTERNAL_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_USERBOOT_INTERNAL_H_

#include <zircon/syscalls/resource.h>

#include <object/handle.h>

HandleOwner get_resource_handle(zx_rsrc_kind_t kind);

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_USERBOOT_INTERNAL_H_
