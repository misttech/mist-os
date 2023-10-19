// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVER_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVER_VISITOR_H_

#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/driver/devicetree/visitors/reference-property.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>

#include <memory>
#include <string_view>

namespace fdf_devicetree {

// DriverVisitor can be used to extract custom properties related to a driver
// out of devicetree by implementing the |DriverVisit| method. Only devicetree
// nodes matching the driver's compatible string is visible to the driver's
// visitor.
class DriverVisitor : public Visitor {
 public:
  using MatchCallback = fit::function<bool(std::string_view)>;

  // Take a callback that matches compatible string for a driver.
  explicit DriverVisitor(MatchCallback compatible_matcher)
      : Visitor(), compatible_matcher_(std::move(compatible_matcher)) {}

  // Overload callback for equality comparison against a specific compatible
  // string.
  explicit DriverVisitor(std::string compatible_string)
      : Visitor(),
        compatible_matcher_([compatible_string](std::string_view compatible_prop) -> bool {
          return compatible_string == compatible_prop;
        }) {}

  ~DriverVisitor() override = default;

  // Allow move construction and assignment.
  DriverVisitor(DriverVisitor&& other) = default;
  DriverVisitor& operator=(DriverVisitor&& other) = default;

  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) final;

  zx::result<> FinalizeNode(Node& node) final;

  void AddReferencePropertyParser(ReferencePropertyParser* reference_parser) {
    ZX_ASSERT(reference_parser != nullptr);
    reference_parsers_.emplace_back(reference_parser);
  }

  virtual zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) = 0;

  virtual zx::result<> DriverFinalizeNode(Node& node) { return zx::ok(); }

 protected:
  bool is_match(const std::unordered_map<std::string_view, devicetree::PropertyValue>& node);

 private:
  MatchCallback compatible_matcher_;
  std::vector<ReferencePropertyParser*> reference_parsers_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVER_VISITOR_H_
