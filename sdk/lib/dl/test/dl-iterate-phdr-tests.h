// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_
#define LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_

#include <lib/elfldltl/layout.h>

#include <filesystem>
#include <span>
#include <vector>

#include "dl-tests-base.h"

namespace dl::testing {

// This is a wrapper around `dl_phdr_info` that makes it easier to
// perform comparisons with other `dl_phdr_info` instances.
class DlPhdrInfo {
 public:
  using Elf = elfldltl::Elf<>;
  using Phdr = Elf::Phdr;

  dl_phdr_info info;

  explicit DlPhdrInfo(dl_phdr_info info) : info(info) {}

  std::string name() const {
    auto path = std::filesystem::path(info.dlpi_name);
    return path.filename();
  }

  // The number of modules that have been loaded at startup or by dlopen.
  constexpr size_t loaded() const { return info.dlpi_adds; }
  // The number of modules that have been unloaded with dlclose.
  constexpr size_t unloaded() const { return info.dlpi_subs; }

  // TODO(https://fxbug.dev/331421403): Check tls_data, tls_modid fields.
  // This will compare module-specific `dl_phdr_info` fields for equality. This
  // does not include equality checks for .dlpi_adds or .dlpi_subs, which are
  // not specific to any one module and are tested outside this operator.
  bool operator==(const DlPhdrInfo& other) const {
    return strcmp(info.dlpi_name, other.info.dlpi_name) == 0 &&
           info.dlpi_addr == other.info.dlpi_addr && info.dlpi_phdr == other.info.dlpi_phdr &&
           info.dlpi_phnum == other.info.dlpi_phnum;
  }

  // Check whether the given `addr` is within the address range of any one of
  // this module's phdrs. This is used to test whether this module is
  // responsible for a given symbol pointer.
  bool contains_addr(uintptr_t addr) const {
    std::span phdrs{reinterpret_cast<const Phdr*>(info.dlpi_phdr), info.dlpi_phnum};
    auto load_bias = info.dlpi_addr;
    for (const Phdr& phdr : phdrs) {
      // Only consider PT_LOAD phdrs in this search.
      if (phdr.type == elfldltl::ElfPhdrType::kLoad) {
        size_t start = load_bias + phdr.vaddr;
        size_t end = start + phdr.memsz;
        if (addr >= start && addr <= end) {
          return true;
        }
      }
    }
    return false;
  }
};

using PhdrInfoList = std::vector<DlPhdrInfo>;

// A `dl_iterate_phdr` callback that collects the dl_iterate_phdr information of
// all loaded modules. This will push an instance of the `DlPhdrInfo` to the
// `PhdrInfo*` that is passed in via the `data` pointer.
inline int CollectPhdrInfo(dl_phdr_info* phdr_info, size_t size, void* data) {
  static_cast<PhdrInfoList*>(data)->emplace_back(*phdr_info);
  return 0;
}

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_
