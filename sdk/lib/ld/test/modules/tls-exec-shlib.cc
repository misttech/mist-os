// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <lib/ld/tls.h>

#include "tls-dep.h"

[[gnu::used, gnu::retain]] alignas(64) thread_local int tls_data = 23;
[[gnu::used, gnu::retain]] thread_local int tls_bss;

extern "C" int64_t TestStart() {
  const auto modules = ld::AbiLoadedModules(ld::abi::_ld_abi);

  const auto& exec_module = *modules.begin();
  const auto& shlib_module = FindTlsDep(modules);

  if (exec_module.tls_modid != 1) {
    return 1;
  }

  if (shlib_module.tls_modid != 2) {
    return 2;
  }

  if (ld::abi::_ld_abi.static_tls_modules.size() != 2) {
    return 3;
  }

  const auto& exec_tls = ld::abi::_ld_abi.static_tls_modules.front();
  const auto& shlib_tls = ld::abi::_ld_abi.static_tls_modules.back();

  if (exec_tls.tls_initial_data.size_bytes() != sizeof(tls_data)) {
    return 4;
  }

  if (shlib_tls.tls_initial_data.size_bytes() != sizeof(tls_dep_data)) {
    return 5;
  }

  if (*reinterpret_cast<const int*>(exec_tls.tls_initial_data.data()) != 23) {
    return 6;
  }

  if (*reinterpret_cast<const int*>(shlib_tls.tls_initial_data.data()) != kTlsDepDataValue) {
    return 7;
  }

  if (exec_tls.tls_bss_size != sizeof(tls_bss)) {
    return 8;
  }

  if (shlib_tls.tls_bss_size != kTlsDepBssSize) {
    return 9;
  }

  if (exec_tls.tls_alignment != 64) {
    return 10;
  }

  if (shlib_tls.tls_alignment != kTlsDepAlign) {
    return 11;
  }

  return 17;
}
