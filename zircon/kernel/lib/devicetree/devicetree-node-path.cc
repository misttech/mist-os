// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/devicetree/path.h"

#include <lib/devicetree/devicetree.h>
#include <zircon/assert.h>

#include <string_view>

namespace devicetree {

CompareResult ComparePath(const NodePath& path_a, const ResolvedPath& path_b) {
  // Compare stem.
  auto [prefix_a_it, prefix_b_it] = internal::CompareRangesOfNodes(
      path_a.begin(), path_a.end(), path_b.Prefix().begin(), path_b.Prefix().end());

  // They both point to the mismatching element.
  if (prefix_a_it != path_a.end() && prefix_b_it != path_b.Prefix().end()) {
    return CompareResult::kIsMismatch;
  }

  if (prefix_b_it != path_b.Prefix().end()) {
    return CompareResult::kIsAncestor;
  }

  auto [suffix_a_it, suffix_b_it] = internal::CompareRangesOfNodes(
      prefix_a_it, path_a.end(), path_b.Suffix().begin(), path_b.Suffix().end());

  // Exhausted the node path components but the stem still has more elements.
  if (suffix_a_it != path_a.end() && suffix_b_it != path_b.Suffix().end()) {
    return CompareResult::kIsMismatch;
  }

  if (suffix_a_it == path_a.end() && suffix_b_it != path_b.Suffix().end()) {
    return CompareResult::kIsAncestor;
  }

  if (suffix_a_it != path_a.end() && suffix_b_it == path_b.Suffix().end()) {
    return CompareResult::kIsDescendant;
  }

  return CompareResult::kIsMatch;
}

CompareResult ComparePath(const NodePath& path_a, std::string_view absolute_path_b) {
  ZX_ASSERT(absolute_path_b.empty() || absolute_path_b[0] == '/');
  return ComparePath(path_a, {.prefix = absolute_path_b});
}

}  // namespace devicetree
