// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONFIG_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONFIG_H_

#include <fbl/vector.h>
#include <ktl/string_view.h>

namespace starnix_kernel_runner {

struct Config {
  fbl::Vector<ktl::string_view> features;
  fbl::Vector<ktl::string_view> init;
  ktl::string_view kernel_cmdline;
  fbl::Vector<ktl::string_view> mounts;
  ktl::string_view name;
  ktl::string_view startup_file_path;
};

}  // namespace starnix_kernel_runner

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_CONFIG_H_
