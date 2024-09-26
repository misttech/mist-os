// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/power/cpp/types.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_power {

ParentElement ParentElement::WithSag(SagElement sag) {
  ValueType value{std::in_place_index<kSagIndex>, std::move(sag)};
  return ParentElement{Type::kSag, std::move(value)};
}

ParentElement ParentElement::WithInstanceName(std::string instance_name) {
  ValueType value{std::in_place_index<kInstanceNameIndex>, std::move(instance_name)};
  return ParentElement{Type::kInstanceName, std::move(value)};
}

void ParentElement::SetSag(SagElement sag) {
  type_ = Type::kSag;
  value_.emplace<kSagIndex>(sag);
}

void ParentElement::SetInstanceName(std::string instance_name) {
  type_ = Type::kInstanceName;
  value_.emplace<kInstanceNameIndex>(std::move(instance_name));
}

std::optional<SagElement> ParentElement::GetSag() const {
  if (type_ != Type::kSag) {
    return std::nullopt;
  }
  ZX_ASSERT_MSG(value_.index() == kSagIndex, "Incorrect variant index: Expected %lu but got %lu",
                kSagIndex, value_.index());
  return std::get<kSagIndex>(value_);
}

std::optional<std::string> ParentElement::GetInstanceName() const {
  if (type_ != Type::kInstanceName) {
    return std::nullopt;
  }
  ZX_ASSERT_MSG(value_.index() == kInstanceNameIndex,
                "Incorrect variant index: Expected %lu but got %lu", kInstanceNameIndex,
                value_.index());
  return std::get<kInstanceNameIndex>(value_);
}

bool ParentElement::operator==(const ParentElement& rhs) const { return value_ == rhs.value_; }

}  // namespace fdf_power

#endif
