// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_MODULE_H_
#define LIB_DL_MODULE_H_

#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/fit/result.h>
#include <lib/ld/load.h>
#include <lib/ld/module.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>

#include "diagnostics.h"

namespace dl {

// TODO(https://fxbug.dev/324136435): These KMax* variables are copied from
// //sdk/lib/ld/startup-load.h, we should share these symbols instead.
// Usually there are fewer than five segments, so this seems like a reasonable
// upper bound to support.
inline constexpr size_t kMaxSegments = 8;

// There can be quite a few metadata phdrs in addition to a PT_LOAD for each
// segment, so allow a fair few more.
inline constexpr size_t kMaxPhdrs = 32;
static_assert(kMaxPhdrs > kMaxSegments);

// TODO(https://fxbug.dev/324136831): comment on how Module relates to
// startup modules when the latter is supported.
// TODO(https://fxbug.dev/328135195): comment on the reference counting when
// that gets implemented.

// A Module is created for every unique ELF file object loaded either
// directly or indirectly as a dependency of another module. It holds the
// ld::abi::Abi<...>::Module data structure that describes the module in the
// passive ABI (see //sdk/lib/ld/module.h).

// A Module is created for the ELF file in `dlopen`, when the
// LoadModule::Load function decodes and loads the file into memory. Whereas a
// LoadModule is ephemeral and lives only as long as it takes to load a module
// and its dependencies in `dlopen`, the Module is a "permanent" data
// structure that is kept alive in the RuntimeDynamicLinker's `loaded_modules`
// list until the module is unloaded.

// While this is an internal API, a Module* is the void* handle returned
// by the public <dlfcn.h> API.
class Module : public fbl::DoublyLinkedListable<std::unique_ptr<Module>> {
 public:
  using Elf = elfldltl::Elf<>;
  using Addr = Elf::Addr;
  using Soname = elfldltl::Soname<>;
  using SymbolInfo = elfldltl::SymbolInfo<Elf>;
  using AbiModule = ld::AbiModule<>;

  // Not copyable, but movable.
  Module(const Module&) = delete;
  Module(Module&&) = default;

  ~Module();

  // TODO(https://fxbug.dev/324136435): This should also match against the
  // SONAME, like ld::LoadModule does.
  constexpr bool operator==(const Soname& other_name) const { return name() == other_name; }

  constexpr const Soname& name() const { return name_; }

  static std::optional<std::unique_ptr<Module>> Create(Soname name, fbl::AllocChecker& ac) {
    auto module = std::unique_ptr<Module>{new (ac) Module};
    if (!ac.check()) {
      return std::nullopt;
    }
    module->name_ = name;
    return std::move(module);
  }

  constexpr AbiModule& module() { return abi_module_; }

  constexpr const AbiModule& module() const { return abi_module_; }

  constexpr Addr load_bias() const { return abi_module_.link_map.addr; }

  const SymbolInfo& symbol_info() const { return abi_module_.symbols; }

 private:
  Module() = default;

  static void Unmap(uintptr_t vaddr, size_t len);
  Soname name_;
  AbiModule abi_module_;
};

// TODO(https://fxbug.dev/324136435): Replace the code and functions copied from
// ld::LoadModule to use them directly; then continue to use methods directly
// from the //sdk/lib/ld public API, refactoring them into shared code as needed.
template <class OSImpl>
class LoadModule {
 public:
  using Loader = typename OSImpl::Loader;
  using File = typename OSImpl::File;
  using Relro = typename Loader::Relro;
  using Elf = elfldltl::Elf<>;
  using Phdr = Elf::Phdr;
  using Ehdr = Elf::Ehdr;
  using Soname = elfldltl::Soname<>;
  using LoadInfo = elfldltl::LoadInfo<Elf, elfldltl::StaticVector<kMaxSegments>::Container>;

  // A LoadModule takes temporary ownership of the Module as it sets requisite
  // information on the object during the loading process. If an error occurs
  // during this process, LoadModule will clean up this module object in its own
  // destruction.
  explicit LoadModule(std::unique_ptr<Module> module) : module_(std::move(module)) {}

  // This must be the last method called on LoadModule and can only be called
  // with `std::move(load_module).take_module();`.
  // Calling this method indicates that the module has been loaded successfully
  // and will give the caller ownership of the module, handing off the
  // responsibility for managing its lifetime and unmapping.
  std::unique_ptr<Module> take_module() && { return std::move(module_); }

  // TODO(https://fxbug.dev/324136435): This is a gutted version of
  // StartupModule::Load, just to get the basic dlopen test case working, and is
  // entirely defined in this header file for convenience for now. In a
  // forthcoming CL we may be able to share a lot of this code with libld.
  [[nodiscard]] bool Load(Diagnostics& diag, File file) {
    // Read the file header and program headers into stack buffers.
    auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
        diag, file, elfldltl::FixedArrayFromFile<Phdr, kMaxPhdrs>{});
    if (!headers) [[unlikely]] {
      return false;
    }

    Loader loader;
    std::optional<Phdr> dyn_phdr;
    auto& [ehdr_owner, phdrs_owner] = *headers;
    const cpp20::span<const Phdr> phdrs = phdrs_owner;
    // Decode phdrs to fill LoadInfo and other things.
    if (!elfldltl::DecodePhdrs(diag, phdrs, elfldltl::PhdrDynamicObserver<Elf>(dyn_phdr),
                               load_info_.GetPhdrObserver(loader.page_size()))) {
      return false;
    }

    if (!loader.Load(diag, load_info_, file.borrow())) [[unlikely]] {
      return false;
    }

    ld::SetModuleVaddrBounds(module_->module(), load_info_, loader.load_bias());

    // TODO(https://fxbug.dev/331804983): After loading is done, split the rest
    // of the logic that performs decoding into a separate function.

    if (DecodeModuleDynamic(module_->module(), diag, loader.memory(), dyn_phdr).is_error())
        [[unlikely]] {
      return false;
    };

    // To finalize the loaded module's mapping, OSImpl::Commit(...) will commit
    // the loader and return the Relro object that is used to apply relro
    // protections later.
    // TODO(https://fxbug.dev/323418587): For now, pass an empty relro_bounds.
    // This will eventually take the decoded relro_phdr.
    loader_relro_ = std::move(loader).Commit(LoadInfo::Region{});

    return true;
  }

 private:
  // TODO(https://fxbug.dev/324136435): This is a modified version of libdl's
  // DecodeModuleDynamic
  template <class Elf = elfldltl::Elf<>, class Diagnostics, class Memory,
            typename... DynamicObservers>
  constexpr fit::result<bool, cpp20::span<const typename Elf::Dyn>> DecodeModuleDynamic(
      Module::AbiModule& module, Diagnostics& diag, Memory& memory,
      const std::optional<typename Elf::Phdr>& dyn_phdr, DynamicObservers&&... dynamic_observers) {
    using Dyn = const typename Elf::Dyn;

    if (!dyn_phdr) [[unlikely]] {
      return fit::error{diag.FormatError("no PT_DYNAMIC program header found")};
    }

    const size_t count = dyn_phdr->filesz / sizeof(Dyn);
    auto read_dyn = memory.template ReadArray<Dyn>(dyn_phdr->vaddr, count);
    if (!read_dyn) [[unlikely]] {
      return fit::error{
          diag.FormatError("cannot read", count, "entries from PT_DYNAMIC",
                           elfldltl::FileAddress{dyn_phdr->vaddr}),
      };
    }

    cpp20::span<const Dyn> dyn = *read_dyn;

    if (!elfldltl::DecodeDynamic(
            diag, memory, dyn, elfldltl::DynamicSymbolInfoObserver(module.symbols),
            std::forward<DynamicObservers>(dynamic_observers)...)) [[unlikely]] {
      return fit::error{false};
    }

    return fit::ok(dyn);
  }

  std::unique_ptr<Module> module_;
  LoadInfo load_info_;
  Relro loader_relro_;
};

}  // namespace dl

#endif  // LIB_DL_MODULE_H_
