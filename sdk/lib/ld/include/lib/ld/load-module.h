// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_LOAD_MODULE_H_
#define LIB_LD_LOAD_MODULE_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/soname.h>
#include <lib/ld/abi.h>
#include <lib/ld/module.h>

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
          LoadModuleInline InlineModule, LoadModuleRelocInfo WithRelocInfo>
class LoadModule {
 public:
  using Elf = ElfLayout;
  using size_type = typename Elf::size_type;
  using Module = typename abi::Abi<Elf>::Module;
  using LoadInfo = elfldltl::LoadInfo<Elf, SegmentContainer>;
  using RelocationInfo = elfldltl::RelocationInfo<Elf>;
  using Soname = elfldltl::Soname<Elf>;
  using Ref = LoadModuleRef<LoadModule>;

  constexpr LoadModule() = default;

  constexpr LoadModule(const LoadModule&) = delete;

  constexpr LoadModule(LoadModule&&) = default;

  // The LoadModule is initially constructed just with a name, which is what
  // will eventually appear in Module::link_map::name.  This is the name by
  // which the module is initially found (in the filesystem or whatever).
  // When the object has a DT_SONAME (Module::soname), this is usually the
  // same; but nothing guarantees that.
  constexpr explicit LoadModule(std::string_view name) : name_(name) {}

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
  constexpr bool operator==(const Soname& name) {
    return name == name_ || (module_ && name == module_->soname);
  }

  // This should be used only after EmplaceModule or (successful) NewModule.
  constexpr Module& module() {
    assert(module_);
    return *module_;
  }
  constexpr const Module& module() const {
    assert(module_);
    return *module_;
  }

  constexpr Soname& soname() const { return module().soname; }

  // This returns an object that can be used like a LoadModule* pointing at
  // this, but is suitable for use in a container like std::unordered_set or
  // fbl::HashTable keyed by soname().
  Ref soname_ref() const { return {this, &LoadModule::soname}; }

  // In an instantiation with InlineModule=kYes, EmplaceModule(..) just
  // constructs Module{...}).
  template <typename... Args, bool Inline = InlineModule == LoadModuleInline::kYes,
            typename = std::enable_if_t<Inline>>
  constexpr void EmplaceModule(Args&&... args) {
    assert(!module_);
    module_.emplace(std::forward<Args>(args)...);
  }

  // In an instantiation with InlineModule=false, NewModule(a..., c...) does
  // new (a...) Module{c...}.  The last argument in a... must be a
  // fbl::AllocChecker that indicates whether `new` succeeded.
  template <typename... NewArgs, bool Inline = InlineModule == LoadModuleInline::kYes,
            typename = std::enable_if_t<!Inline>>
  constexpr void NewModule(NewArgs&&... new_args) {
    assert(!module_);
    module_ = new (std::forward<NewArgs>(new_args)...) Module;
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

  // The following methods satisfy the Module template API for use with
  // elfldltl::ResolverDefinition (see <lib/elfldltl/resolve.h>).

  constexpr auto& symbol_info() const { return module().symbols; }

  constexpr size_type load_bias() const { return module().link_map.addr; }

  // TODO(fxbug.dev/128502): tls methods

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
