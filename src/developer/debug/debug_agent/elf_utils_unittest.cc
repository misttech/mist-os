// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/elf_utils.h"

#include <gtest/gtest.h>

#include "src/developer/debug/debug_agent/zircon_process_handle.h"
#include "src/developer/debug/ipc/records.h"

namespace debug_agent {

namespace {

void ValidateModules(const std::vector<debug_ipc::Module>& modules) {
  // It should contain at least libc, libsyslog, libfdio, vdso and the main executable.
  EXPECT_GT(modules.size(), 5u);

  bool has_libc = false;
  bool has_syslog = false;
  for (const auto& module : modules) {
    if (module.name == "libc.so") {
      has_libc = true;
      EXPECT_FALSE(module.build_id.empty());
    }
    if (module.name == "libsyslog.so") {
      has_syslog = true;
      EXPECT_FALSE(module.build_id.empty());
    }
  }
  EXPECT_TRUE(has_libc);
  EXPECT_TRUE(has_syslog);
}

TEST(ElfUtils, GetElfModulesForProcess) {
  zx::process handle;
  zx::process::self()->duplicate(ZX_RIGHT_SAME_RIGHTS, &handle);
  ZirconProcessHandle self(std::move(handle));

  uintptr_t dl_debug_addr;
  ASSERT_EQ(ZX_OK, zx::process::self()->get_property(ZX_PROP_PROCESS_DEBUG_ADDR, &dl_debug_addr,
                                                     sizeof(dl_debug_addr)));

  ValidateModules(GetElfModulesForProcess(self, dl_debug_addr));
}

TEST(ElfUtils, GetElfModulesForProcessNoDebugAddr) {
  zx::process handle;
  zx::process::self()->duplicate(ZX_RIGHT_SAME_RIGHTS, &handle);
  ZirconProcessHandle self(std::move(handle));

  ValidateModules(GetElfModulesForProcess(self, 0));
}

TEST(ElfUtils, MergeMmapedModules) {
  const char kBinaryName[] = "/home/me/a.out";
  const char kLibCName[] = "/lib/libc.so.6";
  const char kLibFooName[] = "/home/me/libfoo.so";

  constexpr uint64_t kBase1 = 0x1000000;
  constexpr uint64_t kBase2 = 0x2000000;
  constexpr uint64_t kBase3 = 0x3000000;

  std::vector<debug_ipc::Module> modules;
  modules.push_back(debug_ipc::Module{.name = kBinaryName, .base = kBase1, .build_id = "1234"});
  modules.push_back(debug_ipc::Module{.name = kLibCName, .base = kBase2, .build_id = "2345"});

  std::vector<debug_ipc::AddressRegion> maps;
  // A duplicate region for the existing map of a.out.
  maps.push_back(
      debug_ipc::AddressRegion{.name = kBinaryName, .base = kBase1, .size = 0x1000, .read = true});
  maps.push_back(debug_ipc::AddressRegion{
      .name = kBinaryName, .base = kBase1 + 0x1000, .size = 0x1000, .read = true});

  std::vector<elflib::Elf64_Phdr> a_segs{
      {.p_type = elflib::PT_LOAD, .p_vaddr = 0, .p_memsz = 0x1000},
      {.p_type = elflib::PT_LOAD, .p_vaddr = 0x1000, .p_memsz = 0x1000}};

  // A non-readable section shouldn't count.
  maps.push_back(debug_ipc::AddressRegion{.name = "/unreadable", .base = kBase2, .size = 0x1000});

  // This region should be merged in.
  maps.push_back(
      debug_ipc::AddressRegion{.name = kLibFooName, .base = kBase3, .size = 0x1000, .read = true});
  maps.push_back(debug_ipc::AddressRegion{
      .name = kLibFooName, .base = kBase3 + 0x1000, .size = 0x1000, .read = true});

  std::vector<elflib::Elf64_Phdr> foo_segs{
      {.p_type = elflib::PT_LOAD, .p_vaddr = 0, .p_memsz = 0x1000},
      {.p_type = elflib::PT_LOAD, .p_vaddr = 0x1000, .p_memsz = 0x1000}};

  auto get_elf_info = [&](uint64_t base) -> std::optional<internal::ElfSegInfo> {
    switch (base) {
      case kBase1:
      case kBase1 + 0x1000: {
        return internal::ElfSegInfo{
            .segment_headers = a_segs, .so_name = kBinaryName, .build_id = "1234"};
      }
      case kBase2: {
        return std::nullopt;
      }
      case kBase3 + 0x1000:
      case kBase3: {
        return internal::ElfSegInfo{
            .segment_headers = foo_segs, .so_name = kLibFooName, .build_id = "abcd"};
      }
      default:
        ADD_FAILURE() << "Got address 0x" << std::hex << base;
        return {};
    }
  };

  internal::MergeMmapedModules(modules, maps, get_elf_info);

  ASSERT_EQ(3u, modules.size());
  EXPECT_EQ(modules[0].name, kBinaryName);
  EXPECT_EQ(modules[0].base, kBase1);
  EXPECT_EQ(modules[1].name, kLibCName);
  EXPECT_EQ(modules[1].base, kBase2);
  EXPECT_EQ(modules[2].name, kLibFooName);
  EXPECT_EQ(modules[2].base, kBase3);
}

}  // namespace

}  // namespace debug_agent
