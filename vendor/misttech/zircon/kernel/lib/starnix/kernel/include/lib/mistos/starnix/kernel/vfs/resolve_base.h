// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_RESOLVE_BASE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_RESOLVE_BASE_H_

#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>

namespace starnix {

// The lookup is not allowed to traverse any node that's not beneath the specified node.
struct BeneathTag {
  NamespaceNode node;

  bool operator==(const BeneathTag& other) const {
    return node == other.node;
  }
};

// The lookup should be handled as if the root specified node is the file-system root.
struct InRootTag {
  NamespaceNode node;

  bool operator==(const InRootTag& other) const {
    return node == other.node;
  }
};

class ResolveBase {
 public:
  using Variant = ktl::variant<ktl::monostate, BeneathTag, InRootTag>;

  static ResolveBase None() { return ResolveBase({}); }
  static ResolveBase Beneath(NamespaceNode node) { return ResolveBase(BeneathTag{.node = node}); }
  static ResolveBase InRoot(NamespaceNode node) { return ResolveBase(InRootTag{.node = node}); }

  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<>:
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  Variant variant_;

  bool operator!=(const ResolveBase& other) const {
    return variant_ != other.variant_;
  }

 private:
  explicit ResolveBase(Variant variant) : variant_(ktl::move(variant)) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_RESOLVE_BASE_H_
