// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITOR_H_

#include <lib/driver/logging/cpp/logger.h>

namespace fdf_devicetree {
class Node;

// A visitor is a class that visits nodes in the devicetree.
// See |Manager::Walk()| for more information.
class Visitor {
 public:
  explicit Visitor(fdf::Logger* logger) : logger_(logger) {}
  virtual ~Visitor() = default;
  virtual zx::result<> Visit(Node& node) = 0;

 protected:
  // logger_ is here so visitors can use FDF_LOG/FDF_SLOG macros.
  // NOLINTNEXTLINE(misc-non-private-member-variables-in-classes)
  fdf::Logger* logger_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITOR_H_
