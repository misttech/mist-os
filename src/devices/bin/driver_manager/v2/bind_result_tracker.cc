// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/bind_result_tracker.h"

namespace dfv2 {

BindResultTracker::BindResultTracker(size_t expected_result_count,
                                     NodeBindingInfoResultCallback result_callback)
    : expected_result_count_(expected_result_count),
      currently_reported_(0),
      result_callback_(std::move(result_callback)) {
  ZX_ASSERT(expected_result_count > 0);
}

void BindResultTracker::ReportNoBind() {
  size_t current;
  {
    std::scoped_lock guard(lock_);
    currently_reported_++;
    current = currently_reported_;
  }

  Complete(current);
}

void BindResultTracker::ReportSuccessfulBind(const std::string_view& node_name,
                                             const std::string_view& driver) {
  size_t current;
  {
    std::scoped_lock guard(lock_);
    currently_reported_++;
    auto node_binding_info = fuchsia_driver_development::wire::NodeBindingInfo::Builder(arena_)
                                 .node_name(node_name)
                                 .driver_url(driver)
                                 .Build();
    results_.emplace_back(node_binding_info);
    current = currently_reported_;
  }

  Complete(current);
}

void BindResultTracker::ReportSuccessfulBind(
    const std::string_view& node_name,
    const std::vector<fuchsia_driver_development::CompositeInfo>& legacy_composite_infos,
    const std::vector<fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo>&
        composite_spec_infos) {
  size_t current;
  {
    std::scoped_lock guard(lock_);
    currently_reported_++;

    // The wire data in |composite_spec_infos| belongs to an incoming FIDL response so we cannot
    // store it here for ourselves. Therefore this code copies the data over and uses the allocator
    // that is owned here.
    fidl::VectorView<fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo>
        composite_spec_copied(arena_, composite_spec_infos.size());
    for (size_t i = 0; i < composite_spec_infos.size(); i++) {
      composite_spec_copied[i] = fidl::ToWire(arena_, fidl::ToNatural(composite_spec_infos[i]));
    }

    auto node_binding_info = fuchsia_driver_development::wire::NodeBindingInfo::Builder(arena_)
                                 .node_name(node_name)
                                 .legacy_composites(fidl::ToWire(arena_, legacy_composite_infos))
                                 .composite_specs(composite_spec_copied)
                                 .Build();
    results_.emplace_back(node_binding_info);
    current = currently_reported_;
  }

  Complete(current);
}

void BindResultTracker::Complete(size_t current) {
  if (current == expected_result_count_) {
    result_callback_(
        fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>(arena_, results_));
  }
}

}  // namespace dfv2
