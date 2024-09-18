// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_RANGE_MAP_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_RANGE_MAP_H_

#include <lib/mistos/util/allocator.h>
#include <zircon/assert.h>

#include <algorithm>
#include <functional>
#include <iterator>
#include <map>
#include <optional>
#include <utility>
#include <vector>

#include <fbl/vector.h>

namespace util {

template <typename T>
struct Range {
  T start = 0;
  T end = 0;

  bool operator==(Range const& other) const { return (start == other.start) && (end == other.end); }
  bool operator!=(Range const& other) const { return (start != other.start) || (end != other.end); }
  bool operator<(const Range& other) const { return start < other.start; }
  bool contains(const T& key) const { return start <= key && key < end; }
};

template <typename _Type = uint64_t>
class RangeStart {
 public:
  using Type = _Type;
  Range<Type> range;

  explicit RangeStart(const Range<Type>& range) : range(range) {}
  static RangeStart FromPoint(Type point) { return RangeStart({point, point}); }

  bool operator<(const RangeStart& other) const { return range.start < other.range.start; }

  RangeStart() = delete;
};

// A map from a range of keys to values.
//
// At any given time, the map contains a set of non-overlapping, non-empty
// ranges of type K that are associated with values of type V.
//
// A given range can be split into two separate ranges if some of the
// intermediate values are removed from the map of if another value is
// inserted over the intermediate values. When that happens, the value
// for the split range is cloned using the Clone trait.
//
// Adjacent ranges are not merged. Even if the value is "the same" (for some
// definition of "the same"), the ranges are kept separately.
//
// Querying a point in the map returns not only the value stored at that point
// but also the range that value occupies in the map.

template <typename _KeyType = uint64_t, typename _ValueType = int64_t>
class RangeMap {
 public:
  using KeyType = _KeyType;
  using ValueType = _ValueType;
  using RangeType = Range<KeyType>;
  using MapType = std::map<RangeStart<KeyType>, ValueType, std::less<RangeStart<KeyType>>,
                           Allocator<std::pair<const RangeStart<KeyType>, ValueType>>>;
  using IterType = typename MapType::iterator;
  using ConstIterType = typename MapType::const_iterator;

  using IterMapType = std::map<RangeType, ValueType, std::less<RangeType>,
                               Allocator<std::pair<const RangeType, ValueType>>>;

  RangeMap() = default;

  /// Returns the range (and associated value) that contains the given point,
  /// if any.
  ///
  /// At most one range and value can exist at a given point because the
  /// ranges in the map are non-overlapping.
  ///
  /// Empty ranges do not contain any points and therefore cannot be found
  /// using this method. Rather than being stored in the map, values
  /// associated with empty ranges are dropped.
  std::optional<std::pair<RangeType, ValueType>> get(const KeyType& point) const {
    if (map_.empty())
      return std::nullopt;

    // Returns an iterator pointing to the first element in the map that is greater than or equal to
    auto it = map_.lower_bound(RangeStart<KeyType>::FromPoint(point));

    // Checks if the previous element in the map has a key range that contains point. If so, it
    // moves the iterator to the previous element.
    if (it != map_.begin() && std::prev(it)->first.range.end > point) {
      --it;
    }

    // Checks if the current element's key range contains point. If so, it
    // returns a pair of pointers to the key and value.
    if (it != map_.end()) {
      const auto [range_start, value] = *it;
      if (range_start.range.contains(point)) {
        return std::make_pair(range_start.range, value);
      }
    }
    return std::nullopt;
  }

  // Inserts a |range| with the given |value|.
  //
  // The keys included in the given range are now associated with the given
  // value. If those keys were previously associated with another value,
  // are no longer associated with that previous value.
  //
  // This method can cause one or more values in the map to be dropped if
  // the all of the keys associated with those values are contained within
  // the given range.
  //
  // If the inserted range is directly adjacent to another range with an equal value, the
  // inserted range will be merged with the adjacent ranges.
  void insert(Range<KeyType> range, const ValueType& value) {
    if (range.end <= range.start) {
      return;
    }
    remove(range);

    // Check for a range directly before this one. If it exists, it will be the last range with
    // start < range.start.
    auto it = map_.lower_bound(RangeStart<KeyType>::FromPoint(range.start));
    if (it != map_.begin()) {
      const auto [prev_key, prev_value] = *std::prev(it);
      auto prev_range = prev_key.range;
      if (prev_range.end == range.start && prev_value == value) {
        range.start = prev_range.start;
        remove_exact_range(prev_range);
      }
    }

    // Check for a range directly after. If it exists, we can look it up by exact start value
    // of range.end.
    if (auto found = map_.find(RangeStart<KeyType>::FromPoint(range.end)); found != map_.end()) {
      const auto [next_key, next_value] = *found;
      auto next_range = next_key.range;
      if (next_range.start == range.end && next_value == value) {
        range.end = next_range.end;
        remove_exact_range(next_range);
      }
    }

    insert_into_empty_range(range, value);
  }

  // Remove the given range from the map.
  //
  // The keys included in the given range are no longer associated with any
  // values.
  //
  // This method can cause one or more values in the map to be dropped if all of the keys
  // associated with those values are contained within the given range.
  //
  // Returns any removed values.
  fbl::Vector<ValueType> remove(const Range<KeyType>& range) {
    fbl::Vector<ValueType> removed_values;

    // If the given range is empty, there is nothing to do.
    if (range.end <= range.start) {
      return std::move(removed_values);
    }

    // Find the range (if any) that intersects the start of range.
    if (auto pair = get(range.start); pair) {
      auto [old_range, old_value] = *pair;
      if (old_range.contains(range.start)) {
        // Remove that range from the map.
        if (auto value = remove_exact_range(old_range); value.has_value()) {
          fbl::AllocChecker ac;
          removed_values.push_back(value.value(), &ac);
          ZX_ASSERT(ac.check());
        }

        // If the removed range extends after the end of the given range,
        // re-insert the part of the old range that extends beyond the end
        // of the given range.
        if (old_range.end > range.end) {
          insert_into_empty_range({range.end, old_range.end}, old_value);
        }

        // If the removed range extends before the start of the given
        // range, re-insert the part of the old range that extends before
        // the start of the given range.
        if (old_range.start < range.start) {
          insert_into_empty_range({old_range.start, range.start}, old_value);
        }
      }
    }

    // Find the range (if any) that intersects the end of range.
    //
    // There can be at most one such range because we maintain the
    // invariant that the ranges stored in the map are non-overlapping.
    //
    // We exclude the end of the given range because a range that starts
    // exactly at the end of the given range does not overalp the given
    // range.
    auto end_it = map_.lower_bound(RangeStart<KeyType>::FromPoint(range.end));
    if (end_it != map_.begin()) {
      auto [old_range, old_value] = *std::prev(end_it);
      if (old_range.range.contains(range.end)) {
        // Remove that range from the map.
        if (auto value = remove_exact_range(old_range.range); value.has_value()) {
          fbl::AllocChecker ac;
          removed_values.push_back(value.value(), &ac);
          ZX_ASSERT(ac.check());
        }

        // If the removed range extends after the end of the given range,
        // re-insert the part of the old range that extends beyond the end
        // of the given range.
        if (old_range.range.end > range.end) {
          insert_into_empty_range({range.end, old_range.range.end}, old_value);
        }
      }
    }

    // Remove any remaining ranges that are contained within the range.
    //
    // These ranges cannot possibly extend beyond the given range because
    // we will have already removed them from the map at this point.
    //
    // We collect the doomed keys into a Vec to avoid mutating the map
    // during the iteration.
    for (auto it = map_.lower_bound(RangeStart<KeyType>::FromPoint(range.start));
         it != map_.end() && it->first.range.start < range.end;) {
      auto [old_range, old_value] = *it;
      if (old_range.range.end <= range.start) {
        break;
      }
      fbl::AllocChecker ac;
      removed_values.push_back(old_value, &ac);
      ZX_ASSERT(ac.check());
      it = map_.erase(it);
    }

    return std::move(removed_values);
  }

  // Return a new map to iterate over the ranges in the RangeMap.
  IterMapType iter() const {
    IterMapType result;
    for (const auto pair : map_) {
      const auto [range_start, value] = pair;
      result[range_start.range] = value;
    }
    return result;
  }

  // Return a new map to iterate over the ranges in the RangeMap, starting at the first range
  // starting after or at the given point.
  IterMapType iter_starting_at(const KeyType& point) const {
    IterMapType result;
    auto begin = map_.lower_bound(RangeStart<KeyType>::FromPoint(point));
    for (auto it = begin; it != map_.end(); ++it) {
      const auto [range_start, value] = *it;
      result[range_start.range] = value;
    }
    return result;
  }

  // Return a new map to iterate over the ranges in the RangeMap that intersect the requested range.
  template <typename RangeType_>
  IterMapType intersection(const RangeType_& range) const {
    KeyType start = range.start;
    if (auto opt = get(range.start); opt) {
      start = opt->first.start;
    }
    auto begin = map_.lower_bound(RangeStart<KeyType>::FromPoint(start));
    auto end = map_.upper_bound(RangeStart<KeyType>::FromPoint(range.end));

    std::map<RangeType_, ValueType, std::less<RangeType_>,
             Allocator<std::pair<const RangeType_, ValueType>>>
        result;
    for (auto it = begin; it != end; ++it) {
      const auto [range_start, value] = *it;
      result[range_start.range] = value;
    }
    return result;
  }

  void clear() { map_.clear(); }

  [[nodiscard]] bool empty() const { return map_.empty(); }

  [[nodiscard]] size_t size() const { return map_.size(); }

  IterType end() { return map_.end(); }
  ConstIterType end() const { return map_.end(); }

 private:
  // Associate the keys in the given range with the given value.
  //
  // Callers must ensure that the keys in the given range are not already
  // associated with any values.
  void insert_into_empty_range(const Range<KeyType>& range, const ValueType& value) {
    map_[RangeStart<KeyType>(range)] = value;
  }

  // Remove the given range from the map.
  //
  // Callers must ensure that the exact range provided as an argument is
  // contained in the map.
  std::optional<ValueType> remove_exact_range(const Range<KeyType>& range) {
    RangeStart<KeyType> key(range);
    ValueType value = map_[key];
    ASSERT(map_.erase(key));
    return value;
  }

  MapType map_;
};

}  // namespace util

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_RANGE_MAP_H_
