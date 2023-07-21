// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ld/abi.h>
#include <lib/ld/module.h>

#include <type_traits>

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
}

}  // namespace
