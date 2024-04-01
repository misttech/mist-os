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

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_double_list.h>

#include "diagnostics.h"
#include "error.h"

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

// TODO(https://fxbug.dev/324136831): comment on how Module relates to startup
// modules when the latter is supported.
// A Module is created for every unique file object loaded either directly or
// indirectly as a dependency of another Module. While this is an internal API,
// a Module* is the void* handle returned by the public <dlfcn.h> API.
class Module : public fbl::DoublyLinkedListable<std::unique_ptr<Module>> {
 public:
  using Elf = elfldltl::Elf<>;
  using Addr = Elf::Addr;
  using Soname = elfldltl::Soname<>;
  using SymbolInfo = elfldltl::SymbolInfo<Elf>;

  // Not copyable, but movable.
  Module(const Module&) = delete;
  Module(Module&&) = default;

  // The name of the module handle is set to the filename passed to dlopen() to
  // create the module handle. This is usually the same as the DT_SONAME of the
  // AbiModule, but that is not guaranteed. When performing an equality check,
  // match against both possible name values.
  constexpr bool operator==(const Soname& name) const {
    return name == name_ || name == abi_module_.soname;
  }

  constexpr const Soname& name() const { return name_; }

  static std::optional<std::unique_ptr<Module>> Create(Soname name, fbl::AllocChecker& ac) {
    auto module = std::unique_ptr<Module>{new (ac) Module};
    if (!ac.check()) {
      return std::nullopt;
    }
    module->name_ = name;
    return std::move(module);
  }

  void set_load_bias(Addr load_bias) { load_bias_ = load_bias; }

  void set_vaddr_range(Addr vaddr_start, Addr vaddr_end) {
    vaddr_start_ = vaddr_start;
    vaddr_end_ = vaddr_end;
  }

  void set_symbol_info(SymbolInfo symbol_info) { symbol_info_ = symbol_info; }

 private:
  Module() = default;

  // TODO(https://fxbug.dev/324136435): All these fields will be derived from
  // ld::abi::Abi<>::Module when that data structures is supported.
  Soname name_;
  Addr load_bias_;
  Addr vaddr_start_;
  Addr vaddr_end_;
  SymbolInfo symbol_info_;
};

// TODO(https://fxbug.dev/324136435): Replace the code and functions copied from
// ld::LoadModule to use them directly; then continue to use methods directly
// from the //sdk/lib/ld public API, refactoring them into shared code as needed.
template <class OSImpl>
class LoadModule {
 public:
  using Loader = typename OSImpl::Loader;
  using File = typename OSImpl::File;
  using RelroCapability = typename OSImpl::RelroCapability;
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

    SetModuleVaddrBounds(*module_, load_info_, loader.load_bias());

    // TODO(https://fxbug.dev/331804983): After loading is done, split the rest
    // of the logic that performs decoding into a separate function.

    if (DecodeModuleDynamic(*module_, diag, loader.memory(), dyn_phdr).is_error()) [[unlikely]] {
      return false;
    };

    // TODO(https://fxbug.dev/331468188): We should not commit until we've set
    // up unmap on dl::Module destruction, so for now the mapping is thrown
    // away.
    // To finalize the loaded module's mapping, OSImpl::Commit(...) will commit
    // the loader and return the RelroCapability that is used to apply relro
    // protections later.
    // relro_capability_ = OSImpl::Commit(std::move(loader));

    return true;
  }

 private:
  // TODO(https://fxbug.dev/324136435): This is a modified version of libdl's
  // DecodeModuleDynamic
  template <class Elf = elfldltl::Elf<>, class Diagnostics, class Memory,
            typename... DynamicObservers>
  constexpr fit::result<bool, cpp20::span<const typename Elf::Dyn>> DecodeModuleDynamic(
      Module& module, Diagnostics& diag, Memory& memory,
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

    Module::SymbolInfo symbol_info;
    if (!elfldltl::DecodeDynamic(
            diag, memory, dyn, elfldltl::DynamicSymbolInfoObserver(symbol_info),
            std::forward<DynamicObservers>(dynamic_observers)...)) [[unlikely]] {
      return fit::error{false};
    }

    module.set_symbol_info(symbol_info);

    return fit::ok(dyn);
  }

  // TODO(https://fxbug.dev/324136435): This is a modified version of
  // ld::SetModuleVaddrBounds
  constexpr void SetModuleVaddrBounds(Module& module, const LoadInfo& load_info,
                                      typename Elf::size_type load_bias) {
    module.set_load_bias(load_bias);
    auto vaddr_start = load_info.vaddr_start() + load_bias;
    auto vaddr_end = vaddr_start + load_info.vaddr_size();
    module.set_vaddr_range(vaddr_start, vaddr_end);
  }

  std::unique_ptr<Module> module_;
  LoadInfo load_info_;
  RelroCapability relro_capability_;
};

}  // namespace dl

#endif  // LIB_DL_MODULE_H_
