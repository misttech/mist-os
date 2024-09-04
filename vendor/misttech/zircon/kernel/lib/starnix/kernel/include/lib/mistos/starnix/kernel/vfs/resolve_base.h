// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_RESOLVE_BASE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_RESOLVE_BASE_H_

#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>

namespace starnix {

/// Used to specify base directory in `LookupContext` for lookups originating in the `openat2`
/// syscall with either `RESOLVE_BENEATH` or `RESOLVE_IN_ROOT` flag.
enum ResolveBaseType {
  None,

  // The lookup is not allowed to traverse any node that's not beneath the specified node.
  Beneath,

  // The lookup should be handled as if the root specified node is the file-system root.
  InRoot,
};

struct ResolveBase {
  ResolveBaseType type;

  NamespaceNode node;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_RESOLVE_BASE_H_
