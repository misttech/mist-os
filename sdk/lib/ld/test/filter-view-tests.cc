// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/link-map-list.h>
#include <lib/ld/internal/filter-view.h>
#include <lib/ld/module.h>

#include <iterator>
#include <ranges>
#include <utility>

#include <gtest/gtest.h>

namespace {

TEST(LdFilterViewTests, Basic) {
  constexpr std::array arr{1, 5, 7, 2, 8, 43};
  {
    ld::internal::filter_view filter_view(arr, [](auto) { return true; });
    auto it = arr.begin();
    for (int i : filter_view) {
      EXPECT_EQ(i, *it++);
    }
  }
  {
    ld::internal::filter_view filter_view(arr, [](int i) { return i < 6; });
    auto expected = {1, 5, 2};
    auto it = expected.begin();
    for (int i : filter_view) {
      EXPECT_EQ(i, *it++);
    }
  }
}

TEST(LdFilterViewTests, Reverse) {
  constexpr std::array arr{1, 5, 7, 2, 8, 43};
  {
    ld::internal::filter_view filter_view(arr, [](auto) { return true; });

    auto it = filter_view.end();
    for (int expected : {43, 8, 2, 7, 5, 1}) {
      EXPECT_EQ(*--it, expected);
    }
  }
  {
    ld::internal::filter_view filter_view(arr, [](int i) { return i < 6; });

    auto it = filter_view.end();
    for (int expected : {2, 5, 1}) {
      EXPECT_EQ(*--it, expected);
    }
  }
}

using Module = ld::abi::Abi<>::Module;
using ModuleList = elfldltl::LinkMapList<Module, elfldltl::LinkMapListInFirstMemberTraits<Module>>;

template <bool... Visible, size_t... I>
std::array<Module, sizeof...(Visible)> CreateModuleList(std::integer_sequence<bool, Visible...>,
                                                        std::index_sequence<I...>) {
  std::array<Module, sizeof...(Visible)> ret{};

  (
      [&ret] {
        if (I < sizeof...(Visible) - 1) {
          ret[I].link_map.next = &ret[I + 1].link_map;
        }
        if (I) {
          ret[I].link_map.prev = &ret[I - 1].link_map;
        }
        ret[I].symbolizer_modid = I;
        ret[I].symbols_visible = Visible;
      }(),
      ...);

  return ret;
}

template <bool... Visible>
std::array<Module, sizeof...(Visible)> CreateModuleList(
    std::integer_sequence<bool, Visible...> symbols_visible) {
  return CreateModuleList(symbols_visible, std::make_index_sequence<sizeof...(Visible)>{});
}

TEST(LdFilterViewTests, SymbolicModuleView) {
  auto modules = CreateModuleList(std::integer_sequence<bool, 0, 1, 0, 1, 0, 1, 1>{});
  auto view = ModuleList(modules.data());
  auto pred = [](const auto& module) { return module.symbols_visible; };
  ld::internal::filter_view symbolic_view(view, pred);
  EXPECT_EQ(std::distance(symbolic_view.begin(), symbolic_view.end()), 4u);
  constexpr std::array expected_indexes{1u, 3u, 5u, 6u};
  auto expected = expected_indexes.begin();
  for (const Module& module : symbolic_view) {
    EXPECT_TRUE(module.symbols_visible);
    EXPECT_EQ(module.symbolizer_modid, *expected++);
  }
}

}  // namespace
