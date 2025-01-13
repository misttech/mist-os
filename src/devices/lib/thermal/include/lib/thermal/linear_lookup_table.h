// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_THERMAL_INCLUDE_LIB_THERMAL_LINEAR_LOOKUP_TABLE_H_
#define SRC_DEVICES_LIB_THERMAL_INCLUDE_LIB_THERMAL_LINEAR_LOOKUP_TABLE_H_

#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <algorithm>
#include <vector>

namespace linear_lookup_table {

template <typename xType, typename yType>
struct LookupTableEntry {
  xType x;
  yType y;
};

// Both x and y axis must be monotonicly decreasing/increasing.
template <typename xType, typename yType>
class LinearLookupTable {
 private:
  using LookupTableEntryType = LookupTableEntry<xType, yType>;

 public:
  explicit LinearLookupTable(std::vector<LookupTableEntryType> table) : table_(std::move(table)) {
    // Sort profile table descending by x to ensure proper lookup
    auto sort_compare = [](LookupTableEntryType const& lhs,
                           LookupTableEntryType const& rhs) -> bool { return lhs.x > rhs.x; };
    std::sort(table_.begin(), table_.end(), sort_compare);
  }

  explicit LinearLookupTable(std::vector<xType> x_list, std::vector<yType> y_list)
      : LinearLookupTable([&x_list, &y_list]() {
          ZX_ASSERT(x_list.size() == y_list.size());
          std::vector<LookupTableEntryType> table;
          std::transform(x_list.begin(), x_list.end(), y_list.begin(), std::back_inserter(table),
                         [](xType a, yType b) { return LookupTableEntryType{a, b}; });
          return std::move(table);
        }()) {}

  // Always returns float because it may be linearly interpolated.
  zx::result<float> LookupY(float x) const {
    if (table_.empty()) {
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    if (x >= static_cast<float>(table_[0].x)) {
      return zx::ok(static_cast<float>(table_[0].y));
    }
    if (x <= static_cast<float>(table_[table_.size() - 1].x)) {
      return zx::ok(static_cast<float>(table_[table_.size() - 1].y));
    }

    auto lb_compare = [](LookupTableEntryType const& lhs, float val) -> bool {
      return static_cast<float>(lhs.x) > val;
    };
    auto low = std::lower_bound(table_.begin(), table_.end(), x, lb_compare);
    size_t idx = (low - table_.begin());

    return zx::ok(LinearInterpolate(
        static_cast<float>(table_.at(idx).x), static_cast<float>(table_.at(idx).y),
        static_cast<float>(table_.at(idx - 1).x), static_cast<float>(table_.at(idx - 1).y), x));
  }

  // Always returns float because it may be linearly interpolated.
  zx::result<float> LookupX(float y) const {
    if (table_.empty()) {
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    if (y <= static_cast<float>(table_[0].y)) {
      return zx::ok(static_cast<float>(table_[0].x));
    }
    if (y >= static_cast<float>(table_[table_.size() - 1].y)) {
      return zx::ok(static_cast<float>(table_[table_.size() - 1].x));
    }

    auto lb_compare = [](LookupTableEntryType const& lhs, float val) -> bool {
      return static_cast<float>(lhs.y) < val;
    };
    auto lower = std::lower_bound(table_.begin(), table_.end(), y, lb_compare);
    size_t idx = (lower - table_.begin());

    return zx::ok(LinearInterpolate(
        static_cast<float>(table_.at(idx).y), static_cast<float>(table_.at(idx).x),
        static_cast<float>(table_.at(idx - 1).y), static_cast<float>(table_.at(idx - 1).x), y));
  }

 private:
  // Find y for (x, y) on the line of (x1, y1) (x2, y2)
  static constexpr float LinearInterpolate(float x1, float y1, float x2, float y2, float x) {
    float scale = (x - x1) / (x2 - x1);
    return scale * (y2 - y1) + y1;
  }

  std::vector<LookupTableEntryType> table_;
};
}  // namespace linear_lookup_table

#endif  // SRC_DEVICES_LIB_THERMAL_INCLUDE_LIB_THERMAL_LINEAR_LOOKUP_TABLE_H_
