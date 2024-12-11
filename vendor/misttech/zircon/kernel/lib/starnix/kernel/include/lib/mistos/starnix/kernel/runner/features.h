// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_FEATURES_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_FEATURES_H_

#include <lib/mistos/starnix/kernel/runner/container.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>

namespace starnix_kernel_runner {

/// A collection of parsed features and their arguments
struct Features {
  starnix::KernelFeatures kernel_;

  bool selinux = false;

  /// Include the /container directory in the root file system
  bool container = false;

  /// Include the /test_data directory in the root file system
  bool test_data = false;

  /// Include the /custom_artifacts directory in the root file system
  bool custom_artifacts = false;

  bool self_profile = false;
};

fit::result<mtl::Error, Features> parse_features(
    const Config& config /*, const starnix_kernel_structured_config::Config& structured_config*/);

}  // namespace starnix_kernel_runner

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_FEATURES_H_
