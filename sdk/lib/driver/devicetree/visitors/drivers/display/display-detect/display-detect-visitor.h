// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_DISPLAY_DISPLAY_DETECT_DISPLAY_DETECT_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_DISPLAY_DISPLAY_DETECT_DISPLAY_DETECT_VISITOR_H_

#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/driver/devicetree/visitors/property-parser.h>

namespace display_detect_visitor_dt {

class DisplayDetectVisitor : public fdf_devicetree::Visitor {
 public:
  static constexpr char kDisplayDetect[] = "display-detect";
  static constexpr char kDisplayDetectNames[] = "display-detect-names";
  static constexpr char kDisplayDetectCells[] = "#display-detect-cells";

  DisplayDetectVisitor();
  DisplayDetectVisitor(const DisplayDetectVisitor&) = delete;
  DisplayDetectVisitor& operator=(const DisplayDetectVisitor&) = delete;
  ~DisplayDetectVisitor() override = default;

  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

 private:
  bool IsMatch(const std::string& node_name);
  zx::result<> AddChildNodeSpec(fdf_devicetree::Node& child, std::string_view output_name);

  std::unique_ptr<fdf_devicetree::PropertyParser> parser_;
};

}  // namespace display_detect_visitor_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_DISPLAY_DISPLAY_DETECT_DISPLAY_DETECT_VISITOR_H_
