// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <lib/ld/tls.h>
#include <lib/symbolizer-markup/writer.h>

#include <array>
#include <type_traits>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

namespace {

constexpr size_t kPageSize = 0x1000;

using Abi = ld::abi::Abi<>;
static_assert(std::is_default_constructible_v<Abi>);
static_assert(std::is_trivially_copy_constructible_v<Abi>);
static_assert(std::is_trivially_copy_assignable_v<Abi>);
static_assert(std::is_trivially_destructible_v<Abi>);

// Since Module is only used by reference in Abi, it has to be separately
// instantiated and tested.
using Module = Abi::Module;
static_assert(std::is_default_constructible_v<Module>);
static_assert(std::is_trivially_copy_constructible_v<Module>);
static_assert(std::is_trivially_copy_assignable_v<Module>);
static_assert(std::is_trivially_destructible_v<Module>);

using RDebug = Abi::RDebug;
static_assert(std::is_default_constructible_v<RDebug>);
static_assert(std::is_trivially_copy_constructible_v<RDebug>);
static_assert(std::is_trivially_copy_assignable_v<RDebug>);
static_assert(std::is_trivially_destructible_v<RDebug>);

TEST(LdTests, AbiTypes) {
  Abi abi;
  abi = Abi{abi};

  Module module;
  module = Module{module};

  RDebug r_debug;
  r_debug = RDebug{r_debug};

  // Test that this object is zero initialized so it can be put in bss.
  alignas(Module) std::array<std::byte, sizeof(Module)> storage{};
  new (&storage) Module(elfldltl::kLinkerZeroInitialized);
  EXPECT_THAT(storage, testing::Each(testing::Eq(std::byte(0))));
}

constexpr Module MakeModule(uint32_t modid, const char* name, std::span<const std::byte> build_id,
                            Abi::Addr load_addr, std::span<const Abi::Phdr> phdrs,
                            bool symbols_visible, const Module* next, const Module* prev) {
  Module result;
  result.symbolizer_modid = modid;
  result.link_map.name = name;
  result.build_id = build_id;
  result.link_map.addr = load_addr;
  result.vaddr_start = load_addr + phdrs.front().vaddr;
  result.vaddr_end =
      (load_addr + phdrs.back().vaddr + phdrs.back().memsz + kPageSize - 1) & -Abi::Addr{kPageSize};
  result.symbols_visible = symbols_visible;
  result.phdrs = phdrs;
  if (next) {
    result.link_map.next = &const_cast<Module*>(next)->link_map;
  }
  if (prev) {
    result.link_map.prev = &const_cast<Module*>(prev)->link_map;
  }
  return result;
}

constexpr const char* kName1 = "first";
constexpr const char* kName2 = "second";
constexpr const char* kName3 = "third";

constexpr std::array kBuildId1 = {std::byte{0x12}, std::byte{0x34}};
constexpr std::array kBuildId2 = {std::byte{0x56}, std::byte{0x78}};
constexpr std::array kBuildId3 = {std::byte{0xaa}, std::byte{0xbb}, std::byte{0xcc}};

constexpr Abi::Phdr kPhdrs1[] = {
    {
        .type = elfldltl::ElfPhdrType::kLoad,
        .flags = Abi::Phdr::kRead,
        .vaddr = 0,
        .memsz = 0x1000,
    },
    {
        .type = elfldltl::ElfPhdrType::kLoad,
        .flags = Abi::Phdr::kRead | Abi::Phdr::kExecute,
        .vaddr = 0x1000,
        .memsz = 0x2000,
    },
};
constexpr Abi::Phdr kPhdrs2[] = {
    {
        .type = elfldltl::ElfPhdrType::kLoad,
        .flags = Abi::Phdr::kRead | Abi::Phdr::kExecute,
        .vaddr = 0,
        .memsz = 0x1234,
    },
    {
        .type = elfldltl::ElfPhdrType::kLoad,
        .flags = Abi::Phdr::kRead | Abi::Phdr::kWrite,
        .vaddr = 0x2234,
        .memsz = 0x1000,
    },
};
constexpr Abi::Phdr kPhdrs3[] = {
    {
        .type = elfldltl::ElfPhdrType::kLoad,
        .flags = Abi::Phdr::kRead,
        .vaddr = 0,
        .memsz = 0x3000,
    },
    {
        .type = elfldltl::ElfPhdrType::kLoad,
        .flags = Abi::Phdr::kExecute,
        .vaddr = 0x3000,
        .memsz = 0x4000,
    },
    {
        .type = elfldltl::ElfPhdrType::kLoad,
        .flags = Abi::Phdr::kRead | Abi::Phdr::kWrite,
        .vaddr = 0x7000,
        .memsz = 0x8000,
    },
};

constexpr Module kModules[3] = {
    MakeModule(0, kName1, kBuildId1, 0x1000, kPhdrs1, true, &kModules[1], nullptr),
    MakeModule(1, kName2, kBuildId2, 0x2000, kPhdrs2, false, &kModules[2], &kModules[0]),
    MakeModule(2, kName3, kBuildId3, 0x3000, kPhdrs3, true, nullptr, &kModules[1]),
};

TEST(LdTests, AbiModuleList) {
  constexpr Abi abi{.loaded_modules{&kModules[0]}};
  const auto modules = ld::AbiLoadedModules(abi);
  auto it = modules.begin();
  ASSERT_NE(it, modules.end());
  ASSERT_EQ(it++->link_map.name.get(), kName1);
  ASSERT_NE(it, modules.end());
  ASSERT_EQ(it++->link_map.name.get(), kName2);
  ASSERT_NE(it, modules.end());
  ASSERT_EQ(it++->link_map.name.get(), kName3);
  ASSERT_EQ(it, modules.end());
}

TEST(LdTests, AbiSymbolicModuleList) {
  constexpr Abi abi{.loaded_modules{&kModules[0]}};
  auto modules = ld::AbiLoadedSymbolModules(abi);
  auto it = modules.begin();
  ASSERT_NE(it, modules.end());
  const char* c = it++->link_map.name.get();
  ASSERT_EQ(c, kName1);
  ASSERT_NE(it, modules.end());
  ASSERT_EQ(it++->link_map.name.get(), kName3);
  ASSERT_EQ(it, modules.end());
  ASSERT_EQ((--it)->link_map.name.get(), kName3);
  ASSERT_EQ((--it)->link_map.name.get(), kName1);
}

TEST(LdTests, ModuleSymbolizerContext) {
  auto module_context = [](const Module& module) -> std::string {
    std::string markup_text;
    symbolizer_markup::Writer writer{
        [&markup_text](std::string_view str) { markup_text += str; },
    };
    auto& return_ref = ld::ModuleSymbolizerContext(writer, module, kPageSize);
    EXPECT_EQ(&return_ref, &writer);
    return markup_text;
  };

  EXPECT_EQ(module_context(kModules[0]),
            "{{{module:0:first:elf:1234}}}\n"
            "{{{mmap:0x1000:0x1000:load:0:r:0x0}}}\n"
            "{{{mmap:0x2000:0x3000:load:0:rx:0x1000}}}\n");
  EXPECT_EQ(module_context(kModules[1]),
            "{{{module:1:second:elf:5678}}}\n"
            "{{{mmap:0x2000:0x2000:load:1:rx:0x0}}}\n"
            "{{{mmap:0x4000:0x3000:load:1:rw:0x2000}}}\n");
  EXPECT_EQ(module_context(kModules[2]),
            "{{{module:2:third:elf:aabbcc}}}\n"
            "{{{mmap:0x3000:0x3000:load:2:r:0x0}}}\n"
            "{{{mmap:0x6000:0x7000:load:2:x:0x3000}}}\n"
            "{{{mmap:0xa000:0xf000:load:2:rw:0x7000}}}\n");
}

}  // namespace
