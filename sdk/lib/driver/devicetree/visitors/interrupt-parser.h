// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_INTERRUPT_PARSER_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_INTERRUPT_PARSER_H_

#include <lib/driver/devicetree/visitors/reference-property.h>

namespace fdf_devicetree {

class InterruptParser : public ReferencePropertyParser {
 public:
  using ReferencePropertyParser::ReferenceChildCallback;
  using ReferencePropertyParser::ReferenceNodeMatchCallback;

  explicit InterruptParser(ReferenceNodeMatchCallback node_matcher,
                           ReferenceChildCallback child_callback);

  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

 private:
  ReferenceNodeMatchCallback node_matcher_;
  ReferenceChildCallback child_callback_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_INTERRUPT_PARSER_H_
