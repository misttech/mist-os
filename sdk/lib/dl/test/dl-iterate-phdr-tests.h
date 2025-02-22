// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_
#define LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_

#include <lib/elfldltl/layout.h>

#include <algorithm>
#include <filesystem>
#include <format>
#include <span>
#include <vector>

#include "dl-tests-base.h"

namespace dl::testing {

class ModulePhdrInfo {
 public:
  using Elf = elfldltl::Elf<>;
  using Phdr = Elf::Phdr;

  // TODO(https://fxbug.dev/331421403): Support tls_data field.
  explicit constexpr ModulePhdrInfo(const dl_phdr_info& info)
      : name_(info.dlpi_name),
        addr_(info.dlpi_addr),
        phdr_(info.dlpi_phdr),
        phnum_(info.dlpi_phnum),
        tls_modid_(info.dlpi_tls_modid) {}

  constexpr auto operator<=>(const ModulePhdrInfo&) const = default;

  constexpr size_t tls_modid() const { return tls_modid_; }

  std::string basename() const {
    auto path = std::filesystem::path(name_);
    return path.filename();
  }

  // Check whether the given `ptr` is within the address range of any one of
  // this module's phdrs. This is used to test whether this module is
  // responsible for a given symbol pointer.
  bool contains_addr(uintptr_t ptr) const {
    std::span phdrs{reinterpret_cast<const Phdr*>(phdr_), phnum_};
    for (const Phdr& phdr : phdrs) {
      // Only consider PT_LOAD phdrs in this search.
      if (phdr.type == elfldltl::ElfPhdrType::kLoad) {
        size_t start = addr_ + phdr.vaddr;
        size_t end = start + phdr.memsz;
        if (ptr >= start && ptr <= end) {
          return true;
        }
      }
    }
    return false;
  }

 private:
  std::string_view name_;
  uintptr_t addr_;
  const void* phdr_;
  uint16_t phnum_;
  size_t tls_modid_;
};

using ModuleInfoList = std::vector<ModulePhdrInfo>;

// A `dl_iterate_phdr` callback that collects the dl_iterate_phdr information of
// all loaded modules. This will push an instance of the `DlPhdrInfo` to the
// `PhdrInfo*` that is passed in via the `data` pointer.
inline int CollectModulePhdrInfo(dl_phdr_info* phdr_info, size_t size, void* data) {
  static_cast<ModuleInfoList*>(data)->emplace_back(*phdr_info);
  return 0;
}

// Locate the dl_phdr_info for a specific module by its file basename.
template <class Test>
static ModulePhdrInfo GetPhdrInfoForModule(Test* test, std::string_view module_name) {
  ModuleInfoList list;
  EXPECT_EQ(test->DlIteratePhdr(CollectModulePhdrInfo, &list), 0);
  auto info = std::find_if(list.begin(), list.end(), [&](const ModulePhdrInfo& info) {
    return info.basename() == module_name;
  });
  EXPECT_NE(info, list.end());
  return *info;
}

// Used to collect `.dlpi_adds` and `.dlpi_subs` counters.
struct GlobalCounters {
  size_t loaded;
  size_t unloaded;
};

// TODO(https://fxbug.dev/394873439): Test the sequential changes in state of
// these counters.
// Return loaded and unloaded counters; these counters describe the state of the
// number of modules that have been loaded and unloaded when this call was made.
template <class TestFixture>
inline GlobalCounters GetGlobalCounters(TestFixture* test_fixture) {
  // This callback assumes the counters taken from the first dl_phdr_info reflects
  // the true global state, ignoring the possibility of de-synchronization of
  // these counters when another thread dlopen/dlclose and changes these values.
  constexpr auto get_counters = [](dl_phdr_info* phdr_info, size_t size, void* data) {
    GlobalCounters* counters = reinterpret_cast<GlobalCounters*>(data);
    counters->loaded = phdr_info->dlpi_adds;
    counters->unloaded = phdr_info->dlpi_subs;
    // Use the first set of counters we see and halt enumeration.
    return 1;
  };

  GlobalCounters counters;
  EXPECT_EQ(test_fixture->DlIteratePhdr(get_counters, &counters), 1);
  return counters;
}

}  // namespace dl::testing

#endif  // LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_
