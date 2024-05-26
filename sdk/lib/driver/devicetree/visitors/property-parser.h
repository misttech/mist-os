// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_VISITORS_PROPERTY_PARSER_H_
#define LIB_DRIVER_DEVICETREE_VISITORS_PROPERTY_PARSER_H_

#include <lib/driver/devicetree/manager/visitor.h>
#include <lib/fit/function.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstdint>
#include <optional>
#include <utility>
#include <variant>
#include <vector>

namespace fdf_devicetree {

using PropertyName = std::string;
// Cells specific to the property represented in a prop-encoded-array.
using PropertyCells = devicetree::ByteView;

class Property;
using Properties = std::vector<std::unique_ptr<Property>>;

// Values corresponding to properties.
class PropertyValue;
using PropertyValues = std::map<PropertyName, std::vector<PropertyValue>>;

// Helper class to parse properties concerning a visitor. A visitor can bunch all the necessary
// properties it needs to extract from a node and call the |PropertyParser::Parse| method.
// Properties can be a string list, uint32 array or reference properties - see below for classes
// that inherit from |Property| for the complete list of supported property types. The visitor can
// also use this collect all connected properties and associate them appropriately using the vector
// index.
class PropertyParser {
 public:
  explicit PropertyParser(Properties properties) : properties_(std::move(properties)) {}

  virtual ~PropertyParser() = default;

  virtual zx::result<PropertyValues> Parse(Node& node);

 private:
  Properties properties_;
};

// Abstract class to represent a property type.
// To create an instance of Property, use the specific property types.
// Eg: Uint32ArrayProperty, StringListProperty, ReferenceProperty etc.
class Property {
 public:
  explicit Property(PropertyName name, bool required = false)
      : name_(std::move(name)), required_(required) {}

  virtual ~Property() = default;

  virtual zx::result<std::vector<PropertyValue>> Parse(Node& node,
                                                       devicetree::ByteView bytes) const;

  PropertyName name() const { return name_; }

  bool required() const { return required_; }

 private:
  PropertyName name_;
  bool required_;
};

class BoolProperty : public Property {
 public:
  explicit BoolProperty(PropertyName name, bool required = false) : Property(std::move(name)) {}
};

class Uint32Property : public Property {
 public:
  explicit Uint32Property(PropertyName name, bool required = false)
      : Property(std::move(name), required) {}
};

class Uint64Property : public Property {
 public:
  explicit Uint64Property(PropertyName name, bool required = false)
      : Property(std::move(name), required) {}
};

class StringProperty : public Property {
 public:
  explicit StringProperty(PropertyName name, bool required = false)
      : Property(std::move(name), required) {}
};

// Property of uint32 array type.
class Uint32ArrayProperty : public Property {
 public:
  explicit Uint32ArrayProperty(PropertyName name, bool required = false)
      : Property(std::move(name), required) {}

  zx::result<std::vector<PropertyValue>> Parse(Node& node,
                                               devicetree::ByteView bytes) const override;
};

// Property of string list type.
class StringListProperty : public Property {
 public:
  explicit StringListProperty(PropertyName name, bool required = false)
      : Property(std::move(name), required) {}

  zx::result<std::vector<PropertyValue>> Parse(Node& node,
                                               devicetree::ByteView bytes) const override;
};

class ReferenceProperty : public Property {
 public:
  explicit ReferenceProperty(PropertyName name, std::variant<PropertyName, uint32_t> cell_specifier,
                             bool required = false)
      : Property(std::move(name), required), cell_specifier_(std::move(cell_specifier)) {}

  zx::result<std::vector<PropertyValue>> Parse(Node& node,
                                               devicetree::ByteView bytes) const override;

 private:
  std::variant<PropertyName, uint32_t> cell_specifier_;
};

// Class to hold the property value and provide utility functions to access the value appropriately.
class PropertyValue : public devicetree::PropertyValue {
 public:
  explicit PropertyValue(devicetree::ByteView bytes,
                         std::optional<ReferenceNode> parent = std::nullopt)
      : devicetree::PropertyValue(bytes), parent_(parent) {}

  std::optional<std::pair<ReferenceNode, PropertyCells>> AsReference() const;

 private:
  // Parent is present for ReferenceProperty.
  std::optional<ReferenceNode> parent_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_VISITORS_PROPERTY_PARSER_H_
