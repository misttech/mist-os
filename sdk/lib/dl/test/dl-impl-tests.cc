// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Avoid symbol conflict between <ld/abi/abi.h> and <link.h>
#pragma push_macro("_r_debug")
#undef _r_debug
#define _r_debug not_using_system_r_debug
#include <link.h>
#pragma pop_macro("_r_debug")

#include <lib/elfldltl/container.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/testing/diagnostics.h>

#include "dl-impl-tests.h"

namespace dl::testing {
namespace {

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

// Decode an AbiModule from the provided `struct dl_phdr_info`.
AbiModule DecodeModule(const dl_phdr_info& phdr_info) {
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

  // TODO(https://fxbug.dev/331421403): set TLS

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

  return module;
}

int AddModule(struct dl_phdr_info* phdr_info, size_t size, void* data) {
  assert(size >= sizeof(*phdr_info));
  LoadedAbiModulesList* modules = reinterpret_cast<LoadedAbiModulesList*>(data);
  AbiModule module = DecodeModule(*phdr_info);
  modules->push_back(module);
  return 0;
}

// This function decodes the loaded modules at startup to populate a list of
// ld::abi::Abi<>::Module data structures to return to the caller.
LoadedAbiModulesList PopulateLoadedAbiModules() {
  LoadedAbiModulesList modules;
  ZX_ASSERT(!dl_iterate_phdr(AddModule, &modules));

  // Connect the link_map list pointers for each abi module and assign a
  // symbolizer_modid.
  uint32_t symbolizer_modid = 0;
  auto prev = modules.begin();
  for (auto it = std::next(prev); it != modules.end(); ++it) {
    prev->link_map.next = &it->link_map;
    it->link_map.prev = &prev->link_map;
    it->symbolizer_modid = symbolizer_modid++;
    prev = it;
  }

  return modules;
}

}  // namespace

const LoadedAbiModulesList gLoadedAbiModules = PopulateLoadedAbiModules();

}  // namespace dl::testing
