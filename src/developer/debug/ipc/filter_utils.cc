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

const Filter* GetFilterForId(const std::vector<Filter>& filters, uint32_t id) {
  const auto& filter =
      std::ranges::find_if(filters, [id](const Filter& filter) { return id == filter.id; });
  return filter != filters.end() ? &*filter : nullptr;
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

  return std::any_of(components.cbegin(), components.cend(), [&](const ComponentInfo& component) {
    switch (filter.type) {
      case Filter::Type::kComponentName:
        return component.url.substr(component.url.find_last_of('/') + 1) == filter.pattern;
      case Filter::Type::kComponentUrl:
        return MatchComponentUrl(component.url, filter.pattern);
      case Filter::Type::kComponentMoniker:
        return component.moniker == filter.pattern;
      case Filter::Type::kComponentMonikerSuffix:
        return debug::StringEndsWith(component.moniker, filter.pattern);
      case Filter::Type::kComponentMonikerPrefix:
        return debug::StringStartsWith(component.moniker, filter.pattern);
      default:
        return false;
    }
  });
}

bool FilterDefersModules(const Filter* filter) {
  if (filter == nullptr)
    return false;
  return filter->config.weak || filter->config.job_only;
}

std::map<uint64_t, AttachConfig> GetAttachConfigsForFilterMatches(
    const std::vector<FilterMatch>& matches, const std::vector<Filter>& installed_filters) {
  std::map<uint64_t, debug_ipc::AttachConfig> pids_to_attach;

  for (const auto& match : matches) {
    auto matched_filter = GetFilterForId(installed_filters, match.id);

    for (uint64_t matched_pid : match.matched_pids) {
      debug_ipc::AttachConfig config;
      if (matched_filter) {
        config.weak = matched_filter->config.weak;
      }

      if (matched_filter) {
        config.target = matched_filter->config.job_only ? debug_ipc::AttachConfig::Target::kJob
                                                        : debug_ipc::AttachConfig::Target::kProcess;
      }

      auto inserted = pids_to_attach.insert({matched_pid, config});

      // Make sure we double check the mode after the insertion. If the pid had already been
      // added to the map by a weak filter and this is a strong filter that also matched, then we
      // should strongly attach. Conversely, a strong filter should never be overruled by a weak
      // filter. If the filter id for this match is invalid or isn't found, perform a strong
      // attach.
      //
      // TODO(https://fxbug.dev/376247181): revisit how to merge this configuration for job-only
      // filters.
      if (!config.weak)
        inserted.first->second.weak = false;

      // If multiple filters match this pid, they should always have the same attach target. In
      // other words, a job_only filter should only ever cause an attach to a job, and any other
      // filter should always lead us to attach to a process.
      FX_CHECK(inserted.first->second.target == config.target);
    }
  }

  return pids_to_attach;
}

}  // namespace debug_ipc
