// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_LOAD_MODULE_H_
#define LIB_LD_LOAD_MODULE_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/tls-layout.h>
#include <lib/ld/abi.h>
#include <lib/ld/module.h>
#include <lib/ld/tls.h>

#include <cassert>
#include <functional>

#include <fbl/alloc_checker.h>

namespace ld {

// The ld::LoadModule template class provides a base class for a dynamic
// linker's internal data structure describing a module.  This holds the
// ld::abi::Abi<...>::Module object that describes the module in the passive
// ABI, but also other information the only the dynamic linker itself needs.
// It's parameterized by a container template to use in elfldltl::LoadInfo
// (see <lib/elfldltl/load.h> and <lib/elfldltl/container.h>).

// This template parameter indicates whether the ld::abi::Abi<...>::Module is
// stored directly (inline) in the ld::LoadModule or is allocated separately.
// For in-process dynamic linking, it's allocated separately to survive in the
// passive ABI after the LoadModule object is no longer needed.  For other
// cases like out-of-process, that's not needed since publishing its data to
// the passive ABI requires separate work anyway.
enum class LoadModuleInline : bool { kNo, kYes };

// This template parameter indicates whether an elfldltl::RelocationInfo
// object is included.  For simple dynamic linking, the whole LoadModule
// object is ephemeral and discarded shortly after relocation is finished.
// For a zygote model, the LoadModule might be kept indefinitely after
// relocation to be used for repeated loading or symbol resolution.
enum class LoadModuleRelocInfo : bool { kNo, kYes };

// Forward declaration for a helper class defined below.
// See the name_ref() and soname_ref() methods in ld::LoadModule, below.
template <class LoadModule>
class LoadModuleRef;

template <class ElfLayout, template <typename> class SegmentContainer,
          LoadModuleInline InlineModule, LoadModuleRelocInfo WithRelocInfo,
          template <class SegmentType> class SegmentWrapper = elfldltl::NoSegmentWrapper>
class LoadModule {
 public:
  using Elf = ElfLayout;
  using Addr = typename Elf::Addr;
  using size_type = typename Elf::size_type;
  using Module = typename abi::Abi<Elf>::Module;
  using TlsModule = typename abi::Abi<Elf>::TlsModule;
  using LoadInfo =
      elfldltl::LoadInfo<Elf, SegmentContainer, elfldltl::PhdrLoadPolicy::kBasic, SegmentWrapper>;
  using RelocationInfo = elfldltl::RelocationInfo<Elf>;
  using Soname = elfldltl::Soname<Elf>;
  using Ref = LoadModuleRef<LoadModule>;
  using Phdr = typename Elf::Phdr;
  using Sym = typename Elf::Sym;

  constexpr LoadModule() = default;

  constexpr LoadModule(const LoadModule&) = delete;

  constexpr LoadModule(LoadModule&&) = default;

  // The LoadModule is initially constructed just with a name, which is what
  // will eventually appear in Module::link_map::name.  This is the name by
  // which the module is initially found (in the filesystem or whatever).
  // When the object has a DT_SONAME (Module::soname), this is usually the
  // same; but nothing guarantees that.
  constexpr explicit LoadModule(std::string_view name) : name_(name) {}
  constexpr explicit LoadModule(const Soname& name) : name_(name) {}

  constexpr const Soname& name() const { return name_; }

  constexpr void set_name(const Soname& name) { name_ = name; }
  constexpr void set_name(std::string_view name) { name_ = name; }

  // This returns an object that can be used like a LoadModule* pointing at
  // this, but is suitable for use in a container like std::unordered_set or
  // fbl::HashTable keyed by name().
  Ref name_ref() const { return {this, &LoadModule::name}; }

  // For convenient container searches, equality comparison against a (hashed)
  // name checks both name fields.  An unloaded module only has a load name.
  // A loaded module may also have a SONAME.
  constexpr bool operator==(const Soname& name) const {
    return name == name_ || (module_ && name == module_->soname);
  }

  constexpr bool HasModule() const { return static_cast<bool>(module_); }

  // This should be used only after EmplaceModule or (successful) NewModule.
  constexpr Module& module() {
    assert(module_);
    return *module_;
  }
  constexpr const Module& module() const {
    assert(module_);
    return *module_;
  }

  constexpr const Soname& soname() const { return module().soname; }

  // This returns an object that can be used like a LoadModule* pointing at
  // this, but is suitable for use in a container like std::unordered_set or
  // fbl::HashTable keyed by soname().
  Ref soname_ref() const { return {this, &LoadModule::soname}; }

  // In an instantiation with InlineModule=kYes, EmplaceModule(..) just
  // constructs Module{...}).
  template <typename... Args, bool Inline = InlineModule == LoadModuleInline::kYes,
            typename = std::enable_if_t<Inline>>
  constexpr void EmplaceModule(Soname name, uint32_t modid, Args&&... args) {
    assert(!module_);
    module_.emplace(std::forward<Args>(args)...);
    module_->link_map.name = name.c_str();
    module_->symbolizer_modid = modid;
  }

  // In an instantiation with InlineModule=false, NewModule(a..., c...) does
  // new (a...) Module{c...}.  The last argument in a... must be a
  // fbl::AllocChecker that indicates whether `new` succeeded.
  template <typename... Args, bool Inline = InlineModule == LoadModuleInline::kYes,
            typename = std::enable_if_t<!Inline>>
  constexpr void NewModule(Soname name, uint32_t modid, Args&&... args) {
    assert(!module_);
    module_ = new (std::forward<Args>(args)...) Module;
    module_->link_map.name = name.c_str();
    module_->symbolizer_modid = modid;
  }

  LoadInfo& load_info() { return load_info_; }
  const LoadInfo& load_info() const { return load_info_; }

  template <auto R = WithRelocInfo, typename = std::enable_if_t<R == LoadModuleRelocInfo::kYes>>
  RelocationInfo& reloc_info() {
    return reloc_info_;
  }
  template <auto R = WithRelocInfo, typename = std::enable_if_t<R == LoadModuleRelocInfo::kYes>>
  const RelocationInfo& reloc_info() const {
    return reloc_info_;
  }

  template <bool Inline = InlineModule == LoadModuleInline::kYes,
            typename = std::enable_if_t<!Inline>>
  constexpr void set_module(Module& module) {
    module_ = &module;
  }

  // Set up the Abi<>::TlsModule in tls_module() based on the PT_TLS segment.
  template <class Diagnostics, class Memory>
  bool SetTls(Diagnostics& diag, Memory& memory, size_type modid, const Phdr& tls_phdr) {
    using PhdrError = elfldltl::internal::PhdrError<elfldltl::ElfPhdrType::kTls>;

    assert(modid != 0);
    module().tls_modid = modid;

    size_type alignment = std::max<size_type>(tls_phdr.align, 1);
    if (!cpp20::has_single_bit(alignment)) [[unlikely]] {
      if (!diag.FormatError(PhdrError::kBadAlignment)) {
        return false;
      }
    } else {
      tls_module_.tls_alignment = alignment;
    }

    if (tls_phdr.filesz > tls_phdr.memsz) [[unlikely]] {
      if (!diag.FormatError("PT_TLS header `p_filesz > p_memsz`")) {
        return false;
      }
    } else {
      tls_module_.tls_bss_size = tls_phdr.memsz - tls_phdr.filesz;
    }

    auto initial_data = memory.template ReadArray<std::byte>(tls_phdr.vaddr, tls_phdr.filesz);
    if (!initial_data) [[unlikely]] {
      return diag.FormatError("PT_TLS has invalid p_vaddr", elfldltl::FileAddress{tls_phdr.vaddr},
                              " or p_filesz ", tls_phdr.filesz());
    }
    tls_module_.tls_initial_data = *initial_data;

    return true;
  }

  // This returns the TLS module ID assigned by SetTls, or zero if none is set.
  constexpr size_type tls_module_id() const { return module().tls_modid; }

  // This should only be called after SetTls.
  constexpr const TlsModule& tls_module() const {
    assert(tls_module_id() != 0);
    return tls_module_;
  }

  // Use ths TlsLayout object to assign a static TLS offset for this module's
  // PT_TLS segment, if it has one.  SetTls() has already been called if it
  // will be, so the module ID is known.  The two arrays have enough elements
  // for all the module IDs assigned; this sets the slots for this module's ID.
  template <elfldltl::ElfMachine Machine = elfldltl::ElfMachine::kNative, size_type RedZone = 0>
  constexpr void AssignStaticTls(elfldltl::TlsLayout<Elf>& tls_layout,
                                 cpp20::span<TlsModule> tls_modules,
                                 cpp20::span<Addr> tls_offsets) {
    if (tls_module_id() != 0) {
      // These correspond to the p_memsz and p_align of the PT_TLS.
      const size_type memsz = tls_module_.tls_size();
      const size_type align = tls_module_.tls_alignment;

      // Save the offset for use in resolving IE relocations.
      static_tls_bias_ = tls_layout.template Assign<Machine, RedZone>(memsz, align);

      // Fill out the separate TLS module arrays for the passive ABI.
      const size_t idx = tls_module_id() - 1;
      tls_modules[idx] = tls_module_;
      tls_offsets[idx] = static_tls_bias_;
    }
  }

  // The following methods satisfy the Module template API for use with
  // elfldltl::ResolverDefinition (see <lib/elfldltl/resolve.h>).

  constexpr const auto& symbol_info() const { return module().symbols; }

  constexpr size_type load_bias() const { return module().link_map.addr; }

  constexpr bool uses_static_tls() const {
    return (module().symbols.flags() & elfldltl::ElfDynFlags::kStaticTls) ||
           (module().symbols.flags1() & elfldltl::ElfDynFlags1::kPie);
  }

  constexpr size_t static_tls_bias() const { return static_tls_bias_; }

 private:
  using ModuleStorage =
      std::conditional_t<InlineModule == LoadModuleInline::kYes, std::optional<Module>, Module*>;

  struct Empty {};

  using RelocInfoStorage =
      std::conditional_t<WithRelocInfo == LoadModuleRelocInfo::kYes, RelocationInfo, Empty>;

  Soname name_;
  ModuleStorage module_{};
  LoadInfo load_info_;
  [[no_unique_address]] RelocInfoStorage reloc_info_;
  TlsModule tls_module_;
  size_type static_tls_bias_ = 0;
};

// This object is returned by the name_ref() and soname_ref() methods of
// ld::LoadModule.  It's meant to be used in containers keyed by the module
// name.  Both name_ref() and soname_ref() of the same module should be
// inserted into the container so that it can be looked up by either name.
//
// The object can be used like LoadModule* or a similar smart pointer type.
// It also has methods for accessing the module's name (with hash), which is
// either LoadModule::name() or LoadModule::soname(); an operator== that takes
// a Soname (name with hash) object; and it supports both the std::hash and
// <fbl/intrusive_container_utils.h> APIs.  This makes it easy to use in
// containers like std::unordered_set or fbl::HashTable.
template <class LoadModule>
class LoadModuleRef {
 public:
  using Soname = typename LoadModule::Soname;

  constexpr LoadModuleRef() = default;

  constexpr LoadModuleRef(const LoadModuleRef&) = default;

  constexpr LoadModuleRef(LoadModule* module, const Soname& (LoadModule::*name)())
      : module_(module), name_(name) {}

  constexpr LoadModuleRef& operator=(const LoadModuleRef&) = default;

  constexpr LoadModule& operator*() const { return *module_; }

  constexpr LoadModule* operator->() const { return module_; }

  constexpr LoadModule* get() const { return module_; }

  constexpr const Soname& name() const { return module_->*name_(); }

  constexpr uint32_t hash() const { return name().hash(); }

  // This is the API contract used by fbl::HashTable.
  static const Soname& GetKey(const LoadModuleRef& ref) { return ref.name(); }
  static constexpr uint32_t GetHash(const LoadModuleRef& ref) { return ref.hash(); }

  constexpr bool operator==(const Soname& other_name) const { return name() == other_name; }

  constexpr bool operator==(const LoadModuleRef& other) const {
    return module_ == other.module_ || (module_ && other.module_ && *this == other.name());
  }

 private:
  LoadModule* module_ = nullptr;
  const Soname& (LoadModule::*name_)() = nullptr;
};

}  // namespace ld

// This is the API contract for standard C++ hash-based containers.
template <class LoadModule>
struct std::hash<ld::LoadModuleRef<LoadModule>> {
  constexpr uint32_t operator()(ld::LoadModuleRef<LoadModule> ref) const { return ref.hash(); }
};

#endif  // LIB_LD_LOAD_MODULE_H_
