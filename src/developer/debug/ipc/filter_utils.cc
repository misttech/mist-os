// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/ipc/filter_utils.h"

#include <algorithm>

#include "src/developer/debug/ipc/records.h"
#include "src/developer/debug/shared/string_util.h"

namespace debug_ipc {

namespace {

bool MatchComponentUrl(std::string_view url, std::string_view pattern) {
  // Only deals with the most common case: the target URL contains a hash but the pattern doesn't.
  // The hash will look like "?hash=xxx#".
  const char* hash = "?hash=";
  if (url.find(hash) != std::string_view::npos && url.find_last_of('#') != std::string_view::npos &&
      pattern.find(hash) == std::string_view::npos) {
    std::string new_url(url.substr(0, url.find(hash)));
    new_url += url.substr(url.find_last_of('#'));
    return new_url == pattern;
  }
  return url == pattern;
}

}  // namespace

bool FilterMatches(const Filter& filter, const std::string& process_name,
                   const std::vector<ComponentInfo>& components) {
  if (filter.type == Filter::Type::kProcessNameSubstr) {
    return process_name.find(filter.pattern) != std::string::npos;
  } else if (filter.type == Filter::Type::kProcessName) {
    return process_name == filter.pattern;
  } else if (filter.type == Filter::Type::kUnset || filter.type == Filter::Type::kLast) {
    return false;
  }

  return std::any_of(components.cbegin(), components.cend(), [&](const auto& component) {
    switch (filter.type) {
      case Filter::Type::kComponentName:
        return component.url.substr(component.url.find_last_of('/') + 1) == filter.pattern;
      case Filter::Type::kComponentUrl:
        return MatchComponentUrl(component.url, filter.pattern);
      case Filter::Type::kComponentMoniker:
        return component.moniker == filter.pattern;
      case Filter::Type::kComponentMonikerSuffix:
        return debug::StringEndsWith(component.moniker, filter.pattern);
      default:
        return false;
    }
  });
}

}  // namespace debug_ipc
