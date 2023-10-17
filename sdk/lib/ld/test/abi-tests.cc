// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <lib/ld/tls.h>

#include <array>
#include <type_traits>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

namespace {

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

constexpr Module MakeModule(const char* name, bool symbols_visible, const Module* next,
                            const Module* prev) {
  Module result;
  result.link_map.name = name;
  result.symbols_visible = symbols_visible;
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

constexpr Module kModules[3] = {
    MakeModule(kName1, true, &kModules[1], nullptr),
    MakeModule(kName2, false, &kModules[2], &kModules[0]),
    MakeModule(kName3, true, nullptr, &kModules[1]),
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

}  // namespace
