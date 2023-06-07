// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "symbol-tests.h"

#include <array>
#include <set>
#include <vector>

namespace {

constexpr std::string_view kEmpty{};
constexpr elfldltl::SymbolName kEmptySymbol(kEmpty);
constexpr uint32_t kEmptyCompatHash = 0;
constexpr uint32_t kEmptyGnuHash = 5381;

constexpr uint32_t kFoobarCompatHash = 0x06d65882;
constexpr uint32_t kFoobarGnuHash = 0xfde460be;

FORMAT_TYPED_TEST_SUITE(ElfldltlSymbolTests);

TEST(ElfldltlSymbolTests, CompatHash) {
  EXPECT_EQ(kEmptyCompatHash, elfldltl::SymbolName(kEmpty).compat_hash());
  EXPECT_EQ(kFoobarCompatHash, elfldltl::SymbolName(kFoobar).compat_hash());
}

static_assert(kEmptySymbol.compat_hash() == kEmptyCompatHash);
static_assert(kFoobarSymbol.compat_hash() == kFoobarCompatHash);

TEST(ElfldltlSymbolTests, GnuHash) {
  EXPECT_EQ(kEmptyGnuHash, elfldltl::SymbolName(kEmpty).gnu_hash());
  EXPECT_EQ(kFoobarGnuHash, elfldltl::SymbolName(kFoobar).gnu_hash());
}

static_assert(kEmptySymbol.gnu_hash() == kEmptyGnuHash);
static_assert(kFoobarSymbol.gnu_hash() == kFoobarGnuHash);

TYPED_TEST(ElfldltlSymbolTests, CompatHashSize) {
  using Elf = typename TestFixture::Elf;

  elfldltl::SymbolInfo<Elf> si;
  kTestSymbols<Elf>.SetInfo(si);
  si.set_compat_hash(kTestCompatHash<typename Elf::Word>);

  EXPECT_EQ(si.safe_symtab().size(), kTestSymbolCount);
}

TYPED_TEST(ElfldltlSymbolTests, GnuHashSize) {
  using Elf = typename TestFixture::Elf;

  elfldltl::SymbolInfo<Elf> si;
  kTestSymbols<Elf>.SetInfo(si);
  si.set_gnu_hash(kTestGnuHash<typename Elf::Addr>);

  EXPECT_EQ(si.safe_symtab().size(), kTestSymbolCount);
}

TYPED_TEST(ElfldltlSymbolTests, LookupCompatHash) {
  using Elf = typename TestFixture::Elf;

  elfldltl::SymbolInfo<Elf> si;
  kTestSymbols<Elf>.SetInfo(si);
  si.set_compat_hash(kTestCompatHash<typename Elf::Word>);

  EXPECT_EQ(kNotFoundSymbol.Lookup(si), nullptr);

  EXPECT_EQ(kQuuxSymbol.Lookup(si), nullptr);  // Undefined should be skipped.

  const auto* foo = kFooSymbol.Lookup(si);
  ASSERT_NE(foo, nullptr);
  EXPECT_EQ(foo->value(), 1u);

  const auto* bar = kBarSymbol.Lookup(si);
  ASSERT_NE(bar, nullptr);
  EXPECT_EQ(bar->value(), 2u);

  const auto* foobar = kFoobarSymbol.Lookup(si);
  ASSERT_NE(foobar, nullptr);
  EXPECT_EQ(foobar->value(), 3u);
}

TYPED_TEST(ElfldltlSymbolTests, LookupGnuHash) {
  using Elf = typename TestFixture::Elf;

  elfldltl::SymbolInfo<Elf> si;
  kTestSymbols<Elf>.SetInfo(si);
  si.set_gnu_hash(kTestGnuHash<typename Elf::Addr>);

  EXPECT_EQ(kNotFoundSymbol.Lookup(si), nullptr);

  EXPECT_EQ(kQuuxSymbol.Lookup(si), nullptr);  // Undefined should be skipped.

  const auto* foo = kFooSymbol.Lookup(si);
  ASSERT_NE(foo, nullptr);
  EXPECT_EQ(foo->value(), 1u);

  const auto* bar = kBarSymbol.Lookup(si);
  ASSERT_NE(bar, nullptr);
  EXPECT_EQ(bar->value(), 2u);

  const auto* foobar = kFoobarSymbol.Lookup(si);
  ASSERT_NE(foobar, nullptr);
  EXPECT_EQ(foobar->value(), 3u);
}

// The enumeration tests use the same symbol table with both flavors of hash
// table.

template <class Elf>
struct CompatHash {
  using Table = typename elfldltl::CompatHash<Elf>;
  static Table Get(const elfldltl::SymbolInfo<Elf>& si) { return *si.compat_hash(); }
  static constexpr std::string_view kNames[] = {
      "bar",
      "foo",
      "foobar",
      "quux",
  };
};

template <class Elf>
struct GnuHash {
  using Table = typename elfldltl::GnuHash<Elf>;
  static Table Get(const elfldltl::SymbolInfo<Elf>& si) { return *si.gnu_hash(); }
  static constexpr std::string_view kNames[] = {
      // The DT_GNU_HASH table omits the undefined symbols.
      "bar",
      "foo",
      "foobar",
  };
};

template <class Elf, template <class ElfLayout> class HashTable>
void EnumerateHashTable() {
  using HashBucket =
      typename elfldltl::SymbolInfo<Elf>::template HashBucket<typename HashTable<Elf>::Table>;

  elfldltl::SymbolInfo<Elf> si;
  kTestSymbols<Elf>.SetInfo(si);
  si.set_compat_hash(kTestCompatHash<typename Elf::Word>);
  si.set_gnu_hash(kTestGnuHash<typename Elf::Addr>);
  const auto hash_table = HashTable<Elf>::Get(si);

  // Collect all the symbols in a sorted set that doesn't remove duplicates.
  std::multiset<std::string_view> symbol_names;
  for (auto bucket : hash_table) {
    for (uint32_t symndx : HashBucket(hash_table, bucket)) {
      const auto& sym = si.symtab()[symndx];
      std::string_view name = si.string(sym.name);
      ASSERT_FALSE(name.empty());
      symbol_names.insert(name);
    }
  }

  std::vector<std::string_view> sorted_names(symbol_names.begin(), symbol_names.end());
  ASSERT_EQ(sorted_names.size(), std::size(HashTable<Elf>::kNames));
  for (size_t i = 0; i < sorted_names.size(); ++i) {
    EXPECT_EQ(sorted_names[i], HashTable<Elf>::kNames[i]);
  }
}

TYPED_TEST(ElfldltlSymbolTests, EnumerateCompatHash) {
  EnumerateHashTable<typename TestFixture::Elf, CompatHash>();
}

TYPED_TEST(ElfldltlSymbolTests, EnumerateGnuHash) {
  EnumerateHashTable<typename TestFixture::Elf, GnuHash>();
}

TYPED_TEST(ElfldltlSymbolTests, SymbolInfoForSingleLookup) {
  using Elf = typename TestFixture::Elf;

  constexpr static elfldltl::SymbolInfoForSingleLookup<Elf> si{"sym"};

  elfldltl::SymbolName name{si, si.symbol()};
  EXPECT_EQ(name, "sym");
}

TYPED_TEST(ElfldltlSymbolTests, Remote) {
  using Elf = typename TestFixture::Elf;

  using RemoteSymbolInfo = elfldltl::SymbolInfo<Elf, elfldltl::RemoteAbiTraits>;

  RemoteSymbolInfo si;
  si = RemoteSymbolInfo(si);
}

}  // namespace
