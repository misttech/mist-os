// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <stdint.h>

#include <string_view>

extern "C" int64_t TestStart() {
  auto within_module = [](const auto* link_map, auto* addr) {
    using Abi = ld::abi::Abi<>;
    const auto* module = reinterpret_cast<const typename Abi::Module*>(link_map);
    uintptr_t uaddr = reinterpret_cast<uintptr_t>(addr);
    return uaddr >= module->vaddr_start && uaddr <= module->vaddr_end;
  };

  auto* first = &ld::abi::_ld_abi.loaded_modules.get()->link_map;
  if (!first) {
    return 0;
  }

  // Here curr is our executable
  if (std::string_view(first->name.get()) != "") {
    return 1;
  }
  if (!within_module(first, &TestStart)) {
    return 2;
  }

  // The next module should be ld.so
  auto* second = first->next.get();
  if (!second) {
    return 3;
  }

  if (std::string_view(second->name.get()) != ld::abi::kSoname.str()) {
    return 4;
  }
  if (!within_module(second, &ld::abi::_ld_abi)) {
    return 5;
  }

  int additional_modules = 0;
  for (auto* curr = second->next.get(); curr; curr = curr->next.get()) {
    additional_modules++;
  }
  // There should be at most only 1 additional module. The vDSO mauy or may not be present, but
  // no additional modules should be found.
  if (additional_modules > 1) {
    return 6;
  }

  return 17;
}
