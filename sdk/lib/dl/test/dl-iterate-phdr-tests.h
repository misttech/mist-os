// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_
#define LIB_DL_TEST_DL_ITERATE_PHDR_TESTS_H_

#include <lib/elfldltl/layout.h>

#include <algorithm>
#include <filesystem>
#include <format>
#include <ostream>
#include <span>
#include <vector>

#include "dl-tests-base.h"  // This gets <link.h> safely for dl_phdr_info.

namespace dl::testing {

class ModulePhdrInfo {
 public:
  using Elf = elfldltl::Elf<>;
  using Phdr = Elf::Phdr;

  constexpr ModulePhdrInfo() = default;
  constexpr ModulePhdrInfo(const ModulePhdrInfo&) = default;

  explicit ModulePhdrInfo(const dl_phdr_info& info)
      : name_(info.dlpi_name),
        addr_(info.dlpi_addr),
        phdr_(reinterpret_cast<const Phdr*>(info.dlpi_phdr)),
        phnum_(info.dlpi_phnum),
        tls_modid_(info.dlpi_tls_modid),
        tls_data_(info.dlpi_tls_data) {}

  constexpr auto operator<=>(const ModulePhdrInfo&) const = default;

  constexpr std::string_view name() const { return name_; }

  constexpr uintptr_t addr() const { return addr_; }

  constexpr std::span<const Phdr> phdrs() const { return std::span{phdr_, phnum_}; }

  constexpr size_t tls_modid() const { return tls_modid_; }

  constexpr void* tls_data() const { return tls_data_; }

  std::string basename() const {
    auto path = std::filesystem::path(name_);
    return path.filename();
  }

  // Check whether the given `ptr` is within the address range of any one of
  // this module's PT_LOAD segments.  This is used to test whether this module
  // is responsible for a given symbol pointer.
  constexpr bool contains_addr(uintptr_t ptr) const {
    return std::ranges::any_of(phdrs(), [ptr, this](const Phdr& phdr) {
      return phdr.type == elfldltl::ElfPhdrType::kLoad &&  //
             ptr >= addr_ + phdr.vaddr &&                  //
             ptr - addr_ - phdr.vaddr < phdr.memsz;
    });
  }

 private:
  std::string_view name_;
  uintptr_t addr_;
  const Phdr* phdr_;
  uint16_t phnum_;
  size_t tls_modid_;
  void* tls_data_;
};

inline std::ostream& operator<<(std::ostream& os, const ModulePhdrInfo& info) {
  return os << std::format(
             "{{dlpi_name=\"{}\", dlpi_addr={:x}, dlpi_phdr={}, dlpi_phnum={}, dlpi_tls_modid={}, dlpi_tls_data={}}}",
             info.name(), info.addr(), static_cast<const void*>(info.phdrs().data()),
             info.phdrs().size(), info.tls_modid(), info.tls_data());
}

using ModuleInfoList = std::vector<ModulePhdrInfo>;

// A `dl_iterate_phdr` callback that collects the dl_iterate_phdr information
// of all loaded modules. This will push an instance of the `DlPhdrInfo` to the
// `PhdrInfo*` that is passed in via the `data` pointer.
inline int CollectModulePhdrInfo(dl_phdr_info* phdr_info, size_t size, void* data) {
  static_cast<ModuleInfoList*>(data)->emplace_back(*phdr_info);
  return 0;
}

// Locate the dl_phdr_info for a specific module by its file basename.
static ModulePhdrInfo GetPhdrInfoForModule(auto& test, std::string_view module_name) {
  ModuleInfoList list;
  EXPECT_EQ(test.DlIteratePhdr(CollectModulePhdrInfo, &list), 0);
  auto info = std::ranges::find_if(
      list, [&](const ModulePhdrInfo& info) { return info.basename() == module_name; });
  EXPECT_NE(info, list.end());
  return info == list.end() ? ModulePhdrInfo{} : *info;
}

// Used to collect `.dlpi_adds` and `.dlpi_subs` counters.
struct GlobalCounters {
  size_t loaded;
  size_t unloaded;
};

// TODO(https://fxbug.dev/394873439): Test the sequential changes in state of
// these counters.
//
// Return loaded and unloaded counters; these counters describe the state of
// the number of modules that have been loaded and unloaded when this call was
// made.
template <class TestFixture>
inline GlobalCounters GetGlobalCounters(TestFixture* test_fixture) {
  // This callback assumes the counters taken from the first dl_phdr_info
  // reflects the true global state, ignoring the possibility of
  // de-synchronization of these counters when another thread dlopen/dlclose
  // and changes these values.
  auto get_counters = [](dl_phdr_info* phdr_info, size_t size, void* data) {
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
