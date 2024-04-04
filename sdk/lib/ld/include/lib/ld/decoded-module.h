// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_DECODED_MODULE_H_
#define LIB_LD_DECODED_MODULE_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/soname.h>

#include "abi.h"

namespace ld {

// The ld::DecodedModule template class provides a base class for a dynamic
// linker's internal data structure describing a module's ELF metadata.  This
// holds the ld::abi::Abi<...>::Module object that describes the module in the
// passive ABI, but also other information the only the dynamic linker itself
// needs.  It's parameterized by a container template for elfldltl::LoadInfo
// (see <lib/elfldltl/load.h> and <lib/elfldltl/container.h>).
//
// This base class intentionally describes only information that is extracted
// directly from the ELF file's contents; it does not include things like a
// runtime load address, TLS module ID assignment, or even the name by which
// the file was loaded (only any DT_SONAME that might be embedded in the ELF
// metadata itself).  This can be appropriate to use as the base class for
// objects describing a cached ELF file.
//
// See ld::LoadModule (below) for how DecodedModule can be used in a particular
// dynamic linking session.

// This template parameter indicates whether the ld::abi::Abi<...>::Module is
// stored directly (inline) in the ld::DecodedModule or is allocated
// separately.  For in-process dynamic linking, it's allocated separately to
// survive in the passive ABI after the LoadModule object is no longer needed.
// For other cases like out-of-process, that's not needed since publishing its
// data to the passive ABI requires separate work anyway.
enum class AbiModuleInline : bool { kNo, kYes };

// This template parameter indicates whether an elfldltl::RelocationInfo
// object is included.  For simple dynamic linking, the whole LoadModule
// object is ephemeral and discarded shortly after relocation is finished.
// For a zygote model, the LoadModule might be kept indefinitely after
// relocation to be used for repeated loading or symbol resolution.
enum class DecodedModuleRelocInfo : bool { kNo, kYes };

// This is used as a base class for all DecodedModule<...> instantiations just
// as a marker so they can be detected using std::is_base_of.
struct DecodedModuleBase {};

// These fixed limits on the number of PT_LOAD segments and the overall e_phnum
// are what the startup dynamic linker uses, so it's not unreasonable for any
// other implementation to impose the same limits since any ELF file that has
// more segments/phdrs would be unusable as an initial exec module.

// Usually there are fewer than five segments, so this seems like a reasonable
// upper bound to support.
inline constexpr size_t kMaxSegments = 8;

// There can be quite a few metadata phdrs in addition to a PT_LOAD for each
// segment, so allow a fair few more.
inline constexpr size_t kMaxPhdrs = 32;
static_assert(kMaxPhdrs > kMaxSegments);

template <class ElfLayout, template <typename> class SegmentContainer, AbiModuleInline InlineModule,
          DecodedModuleRelocInfo WithRelocInfo = DecodedModuleRelocInfo::kYes,
          template <class SegmentType> class SegmentWrapper = elfldltl::NoSegmentWrapper>
class DecodedModule : public DecodedModuleBase {
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
  using Ehdr = typename Elf::Ehdr;
  using Phdr = typename Elf::Phdr;
  using Sym = typename Elf::Sym;

  static_assert(std::is_move_constructible_v<LoadInfo> || std::is_copy_constructible_v<LoadInfo>);

  static constexpr bool kModuleInline = InlineModule == AbiModuleInline::kYes;
  static constexpr bool kRelocInfo = WithRelocInfo == DecodedModuleRelocInfo::kYes;

  // Derived types might or might not be default-constructed.
  constexpr DecodedModule() = default;

  // Derived types might or might not be copyable and/or movable, depending on
  // the SegmentContainer and SegmentWrapper semantics.
  constexpr DecodedModule(const DecodedModule&) = default;
  constexpr DecodedModule(DecodedModule&&) noexcept = default;
  constexpr DecodedModule& operator=(const DecodedModule&) = default;
  constexpr DecodedModule& operator=(DecodedModule&&) noexcept = default;

  // When a module has been actually decoded at all, this returns true.
  // In default-constructed state, module() cannot be called.  Once
  // HasModule() is true, then module() can be inspected and its
  // contains will be safe, but may be incomplete if decoding hit errors
  // but the Diagnostics object said to keep going.
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

  // **NOTE:** Most methods below use module() and cannot be called unless
  // HasModule() returns true, i.e. after EmplaceModule / NewModule.

  constexpr const auto& symbol_info() const { return module().symbols; }

  // This is the DT_SONAME in the file or empty if there was none (or if
  // decoding was incomplete).  This often matches the name by which the
  // module is known, but need not.  There may be no SONAME at all (normal for
  // an executable or loadable module rather than a shared library), or the
  // embedded SONAME is not the same as the lookup name (file name, usually)
  // used to get the file.
  constexpr const Soname& soname() const { return module().soname; }

  // See <lib/elfldtl/load.h> for details; the LoadInfo provides information
  // on address layout.  This is used to map file segments into memory and/or
  // to convert memory addresses to file offsets.  The SegmentWrapper template
  // class may extend the load_info().segments() element types with holders of
  // segment data mapped or copied from the file (and maybe further prepared).
  constexpr LoadInfo& load_info() { return load_info_; }
  constexpr const LoadInfo& load_info() const { return load_info_; }

  // See <lib/elfldtl/relocation.h> for details; the RelocationInfo provides
  // information need by the <lib/elfldtl/link.h> layer to drive dynamic
  // linking of this module.  It doesn't need to be stored at all if a module
  // is being cached after relocation without concern for a separate dynamic
  // linking session reusing the same data.
  template <auto R = WithRelocInfo, typename = std::enable_if_t<R == DecodedModuleRelocInfo::kYes>>
  constexpr RelocationInfo& reloc_info() {
    return reloc_info_;
  }
  template <auto R = WithRelocInfo, typename = std::enable_if_t<R == DecodedModuleRelocInfo::kYes>>
  constexpr const RelocationInfo& reloc_info() const {
    return reloc_info_;
  }

  // Set up the Abi<>::TlsModule in tls_module() based on the PT_TLS segment.
  // The modid must be nonzero, but its only actual use is in the module() and
  // tls_module_id() values returned by this object.  In a derived object only
  // used as a pure cache of the ELF file's metadata, constant 1 is fine.
  template <class Diagnostics, class Memory>
  bool SetTls(Diagnostics& diag, Memory& memory, const Phdr& tls_phdr, size_type modid) {
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

  // This returns the TLS module ID assigned by SetTls, or zero if none is
  // set.  In a derived type representing a pure cache of the ELF file's
  // metadata, this is just a flag where nonzero indicates it has a PT_TLS.
  // In a derived type for a module in a specific dynamic linking session,
  // this is the ABI's meaning of TLS module ID at runtime.  In either case,
  // it's always nonzero if the module has a PT_TLS and zero if it doesn't.
  constexpr size_type tls_module_id() const { return module().tls_modid; }

  // This should only be called after SetTls.
  constexpr const TlsModule& tls_module() const {
    assert(tls_module_id() != 0);
    return tls_module_;
  }

  template <bool Inline = InlineModule == AbiModuleInline::kYes,
            typename = std::enable_if_t<!Inline>>
  constexpr void set_module(Module& module) {
    module_ = &module;
  }

  // A derived class should wrap EmplaceModule and NewModule with calls to
  // SetAbiName passing the derived class's notion of primary name for the
  // module, so that the AbiModule representation always matches that name.
  constexpr void SetAbiName(const Soname& name) {
    if (module_) {
      module_->link_map.name = name.c_str();
    }
  }

  // In an instantiation with InlineModule=kYes, EmplaceModule(..) just
  // constructs Module{...}).  The modid is always required, though it need
  // not be meaningful.  In a derived type representing a pure cache of the
  // ELF file's metadta, it could be zero in every module.  In a derived type
  // representing a module in a specific dynamic linking scenario, it's useful
  // to assign the ld::abi::Abi::Module::symbolizer_modid value early and rely
  // on it corresponding to the module's order in the linking session's
  // "load-order" list of modules; this allows the index to serve as a means
  // of back-pointer from the module() object to a containing LoadModule sort
  // of object via reference to the session's primary module list.
  template <typename... Args, bool Inline = InlineModule == AbiModuleInline::kYes,
            typename = std::enable_if_t<Inline>>
  constexpr void EmplaceModule(uint32_t modid, Args&&... args) {
    assert(!module_);
    module_.emplace(std::forward<Args>(args)...);
    module_->symbolizer_modid = modid;
  }

  // In an instantiation with InlineModule=false, NewModule(a..., c...) does
  // new (a...) Module{c...}.  See above about the modid argument.  The last
  // argument in a... must be a fbl::AllocChecker that indicates whether `new`
  // succeeded.
  template <typename... Args, bool Inline = InlineModule == AbiModuleInline::kYes,
            typename = std::enable_if_t<!Inline>>
  constexpr void NewModule(uint32_t modid, Args&&... args) {
    assert(!module_);
    module_ = new (std::forward<Args>(args)...) Module;
    module_->symbolizer_modid = modid;
  }

 private:
  using ModuleStorage =
      std::conditional_t<InlineModule == AbiModuleInline::kYes, std::optional<Module>, Module*>;

  struct Empty {};

  using RelocInfoStorage =
      std::conditional_t<WithRelocInfo == DecodedModuleRelocInfo::kYes, RelocationInfo, Empty>;

  ModuleStorage module_{};
  LoadInfo load_info_;
  [[no_unique_address]] RelocInfoStorage reloc_info_;
  TlsModule tls_module_;
};

}  // namespace ld

#endif  // LIB_LD_DECODED_MODULE_H_
