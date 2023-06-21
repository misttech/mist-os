
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_H_

#include <lib/driver/logging/cpp/logger.h>

#include "sdk/lib/driver/devicetree/node.h"
#include "sdk/lib/driver/devicetree/visitor.h"
#include "sdk/lib/driver/devicetree/visitors/bind-property.h"
#include "sdk/lib/driver/devicetree/visitors/mmio.h"

namespace fdf_devicetree {

// A collection of visitors.
// Each visitor's Visit() is invoked during device tree Walk().
template <typename... Visitors>
class MultiVisitor : public Visitor {
 public:
  explicit MultiVisitor() : Visitor() { Init<0, Visitors...>(); }
  ~MultiVisitor() override = default;

  // Allow move construction and assignment.
  MultiVisitor(MultiVisitor&& other) = default;
  MultiVisitor& operator=(MultiVisitor&& other) = default;

  template <int I, typename T, typename... Other>
  constexpr void Init() {
    visitors_[I] = std::unique_ptr<Visitor>(new T());
    if constexpr (sizeof...(Other) > 1) {
      Init<I + 1, Other...>();
    } else if constexpr (sizeof...(Other) == 1) {
      // We use make_unique here so that it deals with the parameter unpacking.
      visitors_[I + 1] = std::unique_ptr<Visitor>(std::make_unique<Other...>().release());
    }
  }

  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
    for (size_t i = 0; i < visitors_.size(); i++) {
      [[maybe_unused]] auto status = visitors_[i]->Visit(node, decoder);
    }
    return zx::ok();
  }

 private:
  std::array<std::unique_ptr<Visitor>, sizeof...(Visitors)> visitors_;
};

using DefaultVisitor = MultiVisitor<BindPropertyVisitor, MmioVisitor>;

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DEFAULT_H_
