// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/elfldltl/container.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/mmap-loader.h>
#include <lib/fit/defer.h>

#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "test-data.h"
#include "tests.h"

namespace {

struct LoadOptions {
  bool commit = true;
  bool reloc = true;
};

class ElfldltlLoaderTests : public testing::Test {
 public:
  void TearDown() override {
    if (!mem_.image().empty()) {
      munmap(mem_.image().data(), mem_.image().size());
    }
  }

  void Load(std::string_view so_path, LoadOptions options = {}) {
    using Phdr = Elf::Phdr;
    using Dyn = Elf::Dyn;

    auto diag = ExpectOkDiagnostics();

    fbl::unique_fd fd = GetTestLib(so_path);
    ASSERT_TRUE(fd) << so_path;

    elfldltl::FdFile fdfile{fd.get(), diag};

    auto headers =
        elfldltl::LoadHeadersFromFile<Elf>(diag, fdfile, elfldltl::NewArrayFromFile<Phdr>());
    ASSERT_TRUE(headers);
    auto& [ehdr, phdrs_result] = *headers;

    ASSERT_TRUE(phdrs_result);
    cpp20::span<const Phdr, cpp20::dynamic_extent> phdrs = phdrs_result.get();

    elfldltl::MmapLoader loader;
    elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container> load_info;
    ASSERT_TRUE(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(loader.page_size())));

    ASSERT_TRUE(loader.Load(diag, load_info, fd.get()));

    elfldltl::DirectMemory& mem = loader.memory();

    std::optional<Phdr> ph;
    elfldltl::DecodePhdrs(diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(ph));
    ASSERT_TRUE(ph);

    auto dyn = mem.ReadArray<Dyn>(ph->vaddr(), ph->filesz() / sizeof(Dyn));
    ASSERT_TRUE(dyn);

    elfldltl::RelocationInfo<Elf> reloc_info;
    ASSERT_TRUE(elfldltl::DecodeDynamic(diag, mem, *dyn,
                                        elfldltl::DynamicRelocationInfoObserver(reloc_info),
                                        elfldltl::DynamicSymbolInfoObserver(sym_info_)));

    if (options.reloc) {
      uintptr_t bias = reinterpret_cast<uintptr_t>(mem.image().data()) - mem.base();
      ASSERT_TRUE(RelocateRelative(mem, reloc_info, bias));

      auto resolve = [this, bias](const auto& ref,
                                  elfldltl::RelocateTls tls_type) -> std::optional<Definition> {
        EXPECT_EQ(tls_type, elfldltl::RelocateTls::kNone) << "Should not have any tls relocs";
        elfldltl::SymbolName name{sym_info_, ref};
        if (const Sym* sym = name.Lookup(sym_info_)) {
          return Definition{sym, bias};
        }
        return {};
      };

      ASSERT_TRUE(elfldltl::RelocateSymbolic(mem, diag, reloc_info, sym_info_, bias, resolve));
    }

    entry_ = mem.GetPointer<std::remove_pointer_t<decltype(entry_)>>(ehdr.entry);
    if (options.commit) {
      mem_.set_image(mem.image());
      mem_.set_base(mem.base());
      std::move(loader).Commit();
    }
    EXPECT_EQ(mem_.image().empty(), !options.commit);
  }

  template <typename T>
  T* entry() const {
    return reinterpret_cast<T*>(entry_);
  }

  template <typename T>
  T* lookup_sym(elfldltl::SymbolName name) {
    auto* sym = name.Lookup(sym_info_);
    EXPECT_NE(sym, nullptr) << name;
    return mem_.GetPointer<T>(sym->value);
  }

 private:
  using Elf = elfldltl::Elf<>;
  using Sym = Elf::Sym;

  struct Definition {
    using size_type = Elf::size_type;

    constexpr bool undefined_weak() const { return false; }

    constexpr const Sym& symbol() const { return *symbol_; }

    constexpr size_type bias() const { return bias_; }

    // These will never actually be called.
    constexpr size_type tls_module_id() const { return 0; }
    constexpr size_type static_tls_bias() const { return 0; }
    constexpr size_type tls_desc_hook() const { return 0; }
    constexpr size_type tls_desc_value() const { return 0; }

    const Sym* symbol_ = nullptr;
    size_type bias_ = 0;
  };

  void (*entry_)() = nullptr;
  elfldltl::DirectMemory mem_;
  elfldltl::SymbolInfo<Elf> sym_info_;
};

TEST_F(ElfldltlLoaderTests, Basic) {
  Load(kRet24, {.reloc = false});

  EXPECT_EQ(entry<decltype(Return24)>()(), 24);
}

TEST_F(ElfldltlLoaderTests, UnmapCorrectly) {
  Load(kRet24, {.commit = false, .reloc = false});

  EXPECT_DEATH(entry<decltype(Return24)>()(), "");
}

TEST_F(ElfldltlLoaderTests, DataSegments) {
  Load(kNoXSegment);

  TestData* data = entry<TestData>();
  EXPECT_EQ(*data->rodata, 5);
  EXPECT_EQ(*data->data, 18);
  EXPECT_EQ(data->data[kSmallDataCount - 1], 1);
  EXPECT_EQ(*data->bss, 0);

  *data->data = 1;
  *data->bss = 2;
  int* rodata = const_cast<int*>(data->rodata);
  EXPECT_DEATH(*rodata = 3, "");
}

TEST_F(ElfldltlLoaderTests, LargeDataSegment) {
  Load(kNoXSegmentLargeData);

  TestData* data = entry<TestData>();
  EXPECT_EQ(*data->rodata, 5);
  EXPECT_EQ(*data->data, 18);
  EXPECT_EQ(data->data[kLargeDataCount - 1], 1);
  EXPECT_EQ(*data->bss, 0);

  *data->data = 1;
  data->data[kLargeDataCount - 1] = 9;
  *data->bss = 2;
  int* rodata = const_cast<int*>(data->rodata);
  EXPECT_DEATH(*rodata = 3, "");
}

TEST_F(ElfldltlLoaderTests, BasicSymbol) {
  Load(kSymbolic);

  constexpr elfldltl::SymbolName kFoo("foo");

  auto* foo_ptr = lookup_sym<decltype(foo)>(kFoo);
  ASSERT_NE(foo_ptr, nullptr);
  EXPECT_EQ(*foo_ptr, 17);
}

TEST_F(ElfldltlLoaderTests, ResolveSymbolic) {
  Load(kSymbolic);

  EXPECT_EQ(lookup_sym<decltype(NeedsPlt)>("NeedsPlt")(), 2);
  EXPECT_EQ(lookup_sym<decltype(NeedsGot)>("NeedsGot")(), 3);
}

}  // namespace
