// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SERIAL_SERIAL_PORT_SERIAL_PORT_VISITOR_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SERIAL_SERIAL_PORT_SERIAL_PORT_VISITOR_H_

#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/driver/devicetree/visitors/property-parser.h>

namespace serial_port_visitor_dt {

class SerialPortVisitor : public fdf_devicetree::Visitor {
 public:
  static constexpr char kSerialport[] = "serial-port";

  SerialPortVisitor();
  zx::result<> Visit(fdf_devicetree::Node& node,
                     const devicetree::PropertyDecoder& decoder) override;

 private:
  std::unique_ptr<fdf_devicetree::PropertyParser> parser_;
};

}  // namespace serial_port_visitor_dt

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_DRIVERS_SERIAL_SERIAL_PORT_SERIAL_PORT_VISITOR_H_
