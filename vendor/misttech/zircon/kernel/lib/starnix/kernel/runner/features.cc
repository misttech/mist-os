// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/runner/features.h"

namespace starnix_kernel_runner {

fit::result<mtl::Error, Features> parse_features(
    const Config& config /*, const starnix_kernel_structured_config::Config& structured_config*/) {
  Features features;

  for (const auto& entry : config.features) {
    ktl::string_view raw_flag;
    ktl::optional<ktl::string_view> raw_args;

    size_t colon_pos = entry.find(':');
    if (colon_pos != ktl::string_view::npos) {
      raw_flag = entry.substr(0, colon_pos);
      raw_args = entry.substr(colon_pos + 1);
    } else {
      raw_flag = entry;
      raw_args = ktl::nullopt;
    }

    if (raw_flag == "container") {
      features.container = true;
    } else if (raw_flag == "custom_artifacts") {
      features.custom_artifacts = true;
    } else if (raw_flag == "enable_suid") {
      features.kernel_.enable_suid = true;
    } else if (raw_flag == "error_on_failed_reboot") {
      features.kernel_.error_on_failed_reboot = true;
    } else if (raw_flag == "selinux") {
      features.selinux = true;
    } else if (raw_flag == "self_profile") {
      features.self_profile = true;
    } else if (raw_flag == "test_data") {
      features.test_data = true;
    } else {
      return fit::error(
          anyhow("Unsupported feature: %.*s", static_cast<int>(raw_flag.size()), raw_flag.data()));
    }
  }

  return fit::ok(ktl::move(features));
}

}  // namespace starnix_kernel_runner
