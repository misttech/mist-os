// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dl-impl-tests.h"

#include <lib/elfldltl/container.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/testing/diagnostics.h>

namespace dl::testing {
namespace {

// A container of abi modules (i.e. startup modules) loaded with this test.
std::vector<AbiModule> gModuleStorage;

using TlsModule = ld::abi::Abi<>::TlsModule;
std::vector<TlsModule> gTlsModuleStorage;

using Offset = ld::abi::Abi<>::Addr;
std::vector<Offset> gTlsOffsetStorage;

struct DecodedModule {
  AbiModule abi_module;
  std::optional<TlsModule> tls_module;
  std::optional<Offset> tls_offset;
};

// This class is a wrapper around the ld::ModuleMemory object that handles
// addresses read from the .dynamic section in memory as modified by glibc.
//
// This "adaptive" memory object will attempt to perform a read on the memory
// object using the initial `ptr` value passed in: if that read fails, it will
// try to read again with `ptr - load_bias`, as is needed for linux.
class AdjustLoadBiasAdaptor : public ld::ModuleMemory {
 public:
  using Base = ld::ModuleMemory;

  explicit AdjustLoadBiasAdaptor(const AbiModule& module, size_t load_bias)
      : Base(module), load_bias_(load_bias) {}

  template <typename T>
  std::optional<std::span<const T>> ReadArray(uintptr_t ptr, size_t count) {
    auto result = Base::ReadArray<T>(ptr, count);
    if (!result) {
      return Base::ReadArray<T>(ptr - load_bias_);
    }
    return result;
  }

  template <typename T>
  std::optional<std::span<const T>> ReadArray(uintptr_t ptr) {
    auto result = Base::ReadArray<T>(ptr);
    if (!result) {
      return Base::ReadArray<T>(ptr - load_bias_);
    }
    return result;
  }

 private:
  Elf::Addr load_bias_ = 0;
};

// TODO(https://fxbug.dev/324136435): Share SetTLs implementation with
// //sdk/lib/ld/include/lib/ld/decoded-module.h
std::optional<TlsModule> SetTls(auto& diag, const Elf::Phdr& tls_phdr, auto& memory) {
  using PhdrError = elfldltl::internal::PhdrError<elfldltl::ElfPhdrType::kTls>;

  Elf::size_type alignment = std::max<Elf::size_type>(tls_phdr.align, 1);
  if (!cpp20::has_single_bit(alignment)) [[unlikely]] {
    diag.FormatError(PhdrError::kBadAlignment);
    return {};
  }
  if (tls_phdr.filesz > tls_phdr.memsz) [[unlikely]] {
    diag.FormatError("PT_TLS header `p_filesz > p_memsz`");
    return {};
  }
  auto initial_data = memory.template ReadArray<std::byte>(tls_phdr.vaddr, tls_phdr.filesz);
  if (!initial_data) [[unlikely]] {
    diag.FormatError("PT_TLS has invalid p_vaddr", elfldltl::FileAddress{tls_phdr.vaddr},
                     " or p_filesz ", tls_phdr.filesz());
    return {};
  }
  return TlsModule{
      .tls_initial_data = *initial_data,
      .tls_bss_size = tls_phdr.memsz - tls_phdr.filesz,
      .tls_alignment = alignment,
  };
}

// Decode an AbiModule from the provided `dl_phdr_info`.
DecodedModule DecodeModule(const dl_phdr_info& phdr_info) {
  static const size_t kPageSize = sysconf(_SC_PAGE_SIZE);

  // Use panic diagnostics to abort and print to stderr in the event any of the
  // following functions fail.
  elfldltl::Diagnostics diag{
      elfldltl::PrintfDiagnosticsReport(__zx_panic, phdr_info.dlpi_name, ": "),
      elfldltl::DiagnosticsPanicFlags(),
  };
  std::optional<Elf::Phdr> dyn_phdr;
  std::optional<Elf::Phdr> tls_phdr;
  elfldltl::Elf<>::size_type vaddr_start, vaddr_size;
  elfldltl::LoadInfo<Elf, elfldltl::StdContainer<std::vector>::Container> load_info;
  std::span phdrs{reinterpret_cast<const Elf::Phdr*>(phdr_info.dlpi_phdr), phdr_info.dlpi_phnum};
  elfldltl::DecodePhdrs(
      diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(dyn_phdr),
      elfldltl::PhdrTlsObserver<Elf>(tls_phdr),
      elfldltl::PhdrLoadObserver<elfldltl::Elf<>>(kPageSize, vaddr_start, vaddr_size),
      load_info.GetPhdrObserver(kPageSize));

  vaddr_start += phdr_info.dlpi_addr;
  AbiModule module{
      .link_map =
          {
              .addr = phdr_info.dlpi_addr,
              .name = elfldltl::AbiPtr<const char>(phdr_info.dlpi_name),
          },
      .vaddr_start = vaddr_start,
      .vaddr_end = vaddr_start + vaddr_size,
      .phdrs = phdrs,
      .tls_modid = phdr_info.dlpi_tls_modid,
  };

  AdjustLoadBiasAdaptor memory(module, phdr_info.dlpi_addr);
  elfldltl::DecodePhdrs(diag, phdrs, PhdrMemoryBuildIdObserver(memory, module));

  auto count = dyn_phdr->filesz() / sizeof(Elf::Dyn);
  std::span dyn = *memory.ReadArray<Elf::Dyn>(dyn_phdr->vaddr, count);
  elfldltl::DecodeDynamic(diag, memory, dyn, elfldltl::DynamicSymbolInfoObserver(module.symbols));

  module.link_map.ld = dyn.data();
  module.soname = module.symbols.soname();

  // All test abi modules that are loaded at startup will have global symbol
  // visibility.
  module.symbols_visible = true;

  std::optional<TlsModule> tls_module;
  std::optional<Offset> tls_offset;
  if (module.tls_modid > 0) {
    assert(tls_phdr);
    tls_module = SetTls(diag, *tls_phdr, memory);
    assert(phdr_info.dlpi_tls_data);
    tls_offset = ld::TpRelativeToOffset(phdr_info.dlpi_tls_data);
  } else {
    assert(!tls_phdr);
  }

  return {.abi_module = module, .tls_module = tls_module, .tls_offset = tls_offset};
}

int AddModule(dl_phdr_info* phdr_info, size_t size, void* data) {
  assert(size >= sizeof(*phdr_info));
  DecodedModule decoded = DecodeModule(*phdr_info);
  gModuleStorage.push_back(decoded.abi_module);
  if (decoded.tls_module) {
    assert(decoded.abi_module.tls_modid > 0);
    ptrdiff_t idx = static_cast<ptrdiff_t>(decoded.abi_module.tls_modid) - 1;
    gTlsModuleStorage.insert(gTlsModuleStorage.begin() + idx, *decoded.tls_module);
    gTlsOffsetStorage.insert(gTlsOffsetStorage.begin() + idx, *decoded.tls_offset);
  }
  return 0;
}

// This function populates an ld::abi::Abi<> object. It iterates over and
// decodes the modules that are loaded with this test program to fill out abi
// information and returns the finished abi object to the caller.
ld::abi::Abi<> PopulateLdAbi() {
  ZX_ASSERT(!dl_iterate_phdr(AddModule, nullptr));

  // Connect the link_map list pointers for each abi module and assign a
  // symbolizer_modid.
  uint32_t symbolizer_modid = 0;
  auto prev = gModuleStorage.begin();
  for (auto it = std::next(prev); it != gModuleStorage.end(); ++it) {
    it->symbolizer_modid = symbolizer_modid++;
    auto& prev_link_map = prev->link_map;
    auto& this_link_map = it->link_map;
    prev_link_map.next = &this_link_map;
    this_link_map.prev = &prev_link_map;
    prev = it;
  }

  return {
      .loaded_modules{&gModuleStorage.front()},
      .static_tls_modules{gTlsModuleStorage},
      .static_tls_offsets{gTlsOffsetStorage},
  };
}

}  // namespace

const ld::abi::Abi<>& gStartupLdAbi = PopulateLdAbi();

}  // namespace dl::testing
