// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_INTERRUPT_PARSER_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_INTERRUPT_PARSER_H_

#include <lib/driver/devicetree/visitors/property-parser.h>

namespace fdf_devicetree {

class InterruptParser : public PropertyParser {
 public:
  static constexpr char kInterruptsExtended[] = "interrupts-extended";
  static constexpr char kInterruptCells[] = "#interrupt-cells";
  static constexpr char kInterrupts[] = "interrupts";

  explicit InterruptParser();
  zx::result<PropertyValues> Parse(Node& node) override;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_INTERRUPT_PARSER_H_
