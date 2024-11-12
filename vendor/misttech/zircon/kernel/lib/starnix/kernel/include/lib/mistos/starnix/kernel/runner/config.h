// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_RUNNER_CONFIG_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_RUNNER_CONFIG_H_

#include <fbl/vector.h>
#include <ktl/string_view.h>

namespace starnix {

struct Config {
  fbl::Vector<ktl::string_view> features;
  fbl::Vector<ktl::string_view> init;
  ktl::string_view kernel_cmdline;
  fbl::Vector<ktl::string_view> mounts;
  ktl::string_view name;
  ktl::string_view startup_file_path;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_RUNNER_CONFIG_H_
