// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/elfldltl/container.h>
#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/mmap-loader.h>
#include <lib/elfldltl/testing/diagnostics.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <lib/fit/defer.h>
#include <sys/mman.h>

#include <filesystem>
#include <vector>

#include <fbl/unique_fd.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#ifdef __Fuchsia__
#include <lib/elfldltl/vmar-loader.h>
#include <lib/elfldltl/vmo.h>
#endif

#include "test-data.h"

namespace {

#ifdef __Fuchsia__
struct LocalVmarLoaderTraits {
  using Loader = elfldltl::LocalVmarLoader;

  static auto MakeLoader() { return elfldltl::LocalVmarLoader{}; }

  static inline auto TestLibProvider = elfldltl::testing::GetTestLibVmo;

  template <class Diagnostics>
  static auto MakeFile(zx::unowned_vmo vmo, Diagnostics& diagnostics) {
    return elfldltl::UnownedVmoFile(vmo->borrow(), diagnostics);
  }

  static zx::unowned_vmo LoadFileArgument(const zx::vmo& vmo) { return vmo.borrow(); }

  static constexpr bool kHasMemory = true;
};

struct RemoteVmarLoaderTraits : public LocalVmarLoaderTraits {
  using Loader = elfldltl::RemoteVmarLoader;

  static auto MakeLoader() { return elfldltl::RemoteVmarLoader{*zx::vmar::root_self()}; }

  static constexpr bool kHasMemory = false;
};
#endif

struct MmapLoaderTraits {
  using Loader = elfldltl::MmapLoader;

  static auto MakeLoader() { return elfldltl::MmapLoader{}; }

  static inline auto TestLibProvider = elfldltl::testing::GetTestLib;

  template <class Diagnostics>
  static auto MakeFile(int fd, Diagnostics& diagnostics) {
    return elfldltl::FdFile(fd, diagnostics);
  }

  static int LoadFileArgument(const fbl::unique_fd& fd) { return fd.get(); }

  static constexpr bool kHasMemory = true;
};

using LoaderTypes = ::testing::Types<
#ifdef __Fuchsia__
    LocalVmarLoaderTraits, RemoteVmarLoaderTraits,
#endif
    MmapLoaderTraits>;

struct LoadOptions {
  bool commit = true;
  bool persist_state = false;
  bool reloc = true;
};

template <class Traits>
class ElfldltlLoaderTests : public testing::Test {
 public:
  void TearDown() override {
    // We use the Posix APIs for genericism of the test.
    // The destructors of the various loaders use platform
    // specific APIs.
    if (!mem_.image().empty()) {
      munmap(mem_.image().data(), mem_.image().size());
    }
  }

  void Load(std::string_view so_path, LoadOptions options = {}) {
    auto diag = elfldltl::testing::ExpectOkDiagnostics();

    auto lib_file = Traits::TestLibProvider(so_path);
    if (HasFatalFailure()) {
      return;
    }

    auto file = Traits::MakeFile(Traits::LoadFileArgument(lib_file), diag);

    auto headers =
        elfldltl::LoadHeadersFromFile<Elf>(diag, file, elfldltl::NewArrayFromFile<Phdr>());
    ASSERT_TRUE(headers);
    auto& [ehdr, phdrs_result] = *headers;

    ASSERT_TRUE(phdrs_result);
    cpp20::span<const Phdr> phdrs = phdrs_result.get();

    typename Traits::Loader loader = Traits::MakeLoader();

    ElfLoadInfo load_info;
    ASSERT_TRUE(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(loader.page_size())));

    ASSERT_TRUE(loader.Load(diag, load_info, Traits::LoadFileArgument(lib_file)));

    elfldltl::DirectMemory mem;
    if constexpr (Traits::kHasMemory) {
      mem.set_base(loader.memory().base());
      mem.set_image(loader.memory().image());
    } else {
      mem.set_base(load_info.vaddr_start());
      mem.set_image({reinterpret_cast<std::byte*>(load_info.vaddr_start() + loader.load_bias()),
                     load_info.vaddr_size()});
    }

    EXPECT_EQ(mem.base(), load_info.vaddr_start());
    EXPECT_EQ(mem.image().size_bytes(), load_info.vaddr_size());
    EXPECT_EQ(reinterpret_cast<uintptr_t>(mem.image().data()),
              load_info.vaddr_start() + loader.load_bias());

    std::optional<Phdr> ph;
    std::optional<Phdr> relro_phdr;
    elfldltl::DecodePhdrs(diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(ph),
                          elfldltl::PhdrRelroObserver<Elf>(relro_phdr));
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

    if (options.persist_state) {
      loader_opt_ = std::move(loader);
      relro_phdr_ = std::move(relro_phdr);
    }

    EXPECT_EQ(mem_.image().empty(), !options.commit);
  }

  template <typename T>
  T* lookup_sym(elfldltl::SymbolName name) {
    auto* sym = name.Lookup(sym_info_);
    EXPECT_NE(sym, nullptr) << name;
    return mem_.GetPointer<T>(sym->value);
  }

  void protect_relro() {
    ASSERT_TRUE(loader_opt_);
    ASSERT_TRUE(relro_phdr_);
    auto diag = elfldltl::testing::ExpectOkDiagnostics();

    ASSERT_TRUE(loader_opt_->ProtectRelro(
        diag, ElfLoadInfo::RelroBounds(*relro_phdr_, loader_opt_->page_size())));
  }

  template <typename T>
  T* entry() const {
    return reinterpret_cast<T*>(entry_);
  }

 private:
  using Elf = elfldltl::Elf<>;
  using ElfLoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container>;
  using Phdr = Elf::Phdr;
  using Dyn = Elf::Dyn;
  using Sym = Elf::Sym;
  using size_type = Elf::size_type;

  struct Definition {
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

  std::optional<typename Traits::Loader> loader_opt_;
  std::optional<Phdr> relro_phdr_;
};

TYPED_TEST_SUITE(ElfldltlLoaderTests, LoaderTypes);

TYPED_TEST(ElfldltlLoaderTests, Basic) {
  this->Load(kRet24, {.reloc = false});

  EXPECT_EQ(this->template entry<decltype(Return24)>()(), 24);
}

TYPED_TEST(ElfldltlLoaderTests, UnmapCorrectly) {
  this->Load(kRet24, {.commit = false, .reloc = false});

  EXPECT_DEATH(this->template entry<decltype(Return24)>()(), "");
}

TYPED_TEST(ElfldltlLoaderTests, PersistCorrectly) {
  this->Load(kRet24, {.commit = false, .persist_state = true, .reloc = false});

  EXPECT_EQ(this->template entry<decltype(Return24)>()(), 24);
}

TYPED_TEST(ElfldltlLoaderTests, DataSegments) {
  this->Load(kNoXSegment);

  TestData* data = this->template entry<TestData>();
  EXPECT_EQ(*data->rodata, 5);
  EXPECT_EQ(*data->data, 18);
  EXPECT_EQ(data->data[kSmallDataCount - 1], 1);
  EXPECT_EQ(*data->bss, 0);

  *data->data = 1;
  *data->bss = 2;
  int* rodata = const_cast<int*>(data->rodata);
  EXPECT_DEATH(*rodata = 3, "");
}

TYPED_TEST(ElfldltlLoaderTests, LargeDataSegment) {
  this->Load(kNoXSegmentLargeData);

  TestData* data = this->template entry<TestData>();
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

TYPED_TEST(ElfldltlLoaderTests, LargeBssSegment) {
  this->Load(kNoXSegmentLargeBss);

  TestData* data = this->template entry<TestData>();
  EXPECT_EQ(*data->rodata, 5);
  EXPECT_EQ(*data->data, 18);
  EXPECT_EQ(data->data[kSmallDataCount - 1], 1);
  cpp20::span bss(data->bss, kLargeBssCount);
  EXPECT_THAT(bss, testing::Each(testing::Eq(0)));

  *data->data = 1;
  *data->bss = 2;
  int* rodata = const_cast<int*>(data->rodata);
  EXPECT_DEATH(*rodata = 3, "");
}

TYPED_TEST(ElfldltlLoaderTests, ProtectRelroTest) {
  this->Load(kRelro, {.commit = false, .persist_state = true});

  RelroData* data = this->template entry<RelroData>();
  ASSERT_NE(data, nullptr);

  int* volatile relrodata = const_cast<int*>(data->relocated);
  ASSERT_NE(relrodata, nullptr);

  data->relocated = nullptr;
  this->protect_relro();

  ASSERT_EQ(data->relocated, nullptr);
  EXPECT_DEATH(data->relocated = relrodata, "");
}

TYPED_TEST(ElfldltlLoaderTests, BasicSymbol) {
  this->Load(kSymbolic);

  constexpr elfldltl::SymbolName kFoo("foo");

  auto* foo_ptr = this->template lookup_sym<decltype(foo)>(kFoo);
  ASSERT_NE(foo_ptr, nullptr);
  EXPECT_EQ(*foo_ptr, 17);
}

TYPED_TEST(ElfldltlLoaderTests, ResolveSymbolic) {
  this->Load(kSymbolic);

  EXPECT_EQ(this->template lookup_sym<decltype(NeedsPlt)>("NeedsPlt")(), 2);
  EXPECT_EQ(this->template lookup_sym<decltype(NeedsGot)>("NeedsGot")(), 3);
}

}  // namespace
