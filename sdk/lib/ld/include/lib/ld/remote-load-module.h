// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_REMOTE_LOAD_MODULE_H_
#define LIB_LD_REMOTE_LOAD_MODULE_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/loadinfo-mapped-memory.h>
#include <lib/elfldltl/loadinfo-mutable-memory.h>
#include <lib/elfldltl/mapped-vmo-file.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/resolve.h>
#include <lib/elfldltl/segment-with-vmo.h>
#include <lib/elfldltl/soname.h>
#include <lib/fit/result.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>

#include <algorithm>
#include <vector>

#include "internal/filter-view.h"

namespace ld {

// ld::RemoteDecodedModule represents an ELF file and all the metadata
// extracted from it.  It's specifically meant only to hold a cache of
// information distilled purely from the file's contents.  So it doesn't
// include a name, runtime load address, symbolizer module ID, or TLS module
// ID.  The tls_module_id() method returns 1 if the module has a PT_TLS at all.
//
// The RemoteDecodedModule object owns a read-and-execute-only VMO handle for
// the file's immutable contents and a mapping covering all its segments
// (perhaps the whole file).  The VMO is supplied at construction.
//
// It's a movable object, but moving it does not invalidate all the metadata
// pointers.  For the lifetime of the RemoteDecodedModule, other objects can
// point into the mapped file's metadata such as by doing shallow copies of
// `.module()`.  The `.load_info()` object may own move-only zx::vmo handles to
// VMOs in `.segments()` via elfldltl::SegmentWithVmo::NoCopy.  (The
// distinction between NoCopy and Copy doesn't really matter here, since the
// segments in RemoteDecodedModule should never be passed to a VmarLoader.)  As
// no relocations are performed on these segments, such a writable VMO will
// only exist when elfldltl::SegmentWithVmo::AlignSegments finds a
// DataWithZeroFillSegment with a partial page of bss to be cleared.

template <class Elf>
using RemoteDecodedModuleBase =
    DecodedModule<Elf, elfldltl::StdContainer<std::vector>::Container, AbiModuleInline::kYes,
                  DecodedModuleRelocInfo::kYes, elfldltl::SegmentWithVmo::NoCopy>;

template <class Elf = elfldltl::Elf<>>
class RemoteDecodedModule : public RemoteDecodedModuleBase<Elf> {
 public:
  using Base = RemoteDecodedModuleBase<Elf>;
  static_assert(std::is_move_constructible_v<Base>);
  static_assert(std::is_move_assignable_v<Base>);

  using typename Base::LoadInfo;
  using typename Base::Phdr;
  using typename Base::size_type;
  using typename Base::Soname;
  using Ehdr = typename Elf::Ehdr;

  // Names of each DT_NEEDED entry for the module.
  using NeededList = std::vector<Soname>;

  // Information from decoding a main executable, specifically.  This
  // information may exist in any file, but it's only of interest when
  // launching a main executable.
  struct ExecInfo {
    size_type relative_entry = 0;         // File-relative entry point address.
    std::optional<size_type> stack_size;  // Any requested initial stack size.
  };

  // This is the Memory API object returned by memory_metadata(), below.
  using MetadataMemory = elfldltl::LoadInfoMappedMemory<LoadInfo, elfldltl::MappedVmoFile>;

  // A default-constructed object is just an empty placeholder that can be
  // move-assigned.  An empty object (where `!this->vmo()`) could be used as a
  // negative cache entry in a file identity -> RemoteDecodedModule map without
  // holding onto a VMO handle for the invalid file.
  RemoteDecodedModule() = default;

  // RemoteDecodedModule is move-constructible and move-assignable.
  RemoteDecodedModule(RemoteDecodedModule&&) = default;

  // After construction, Init should be called to do the actual decoding.
  explicit RemoteDecodedModule(zx::vmo vmo) : vmo_(std::move(vmo)) {}

  RemoteDecodedModule& operator=(RemoteDecodedModule&&) = default;

  // The VMO can be used or borrowed during the lifetime of this object.
  // Before Init, this is the only method that will return non-empty data.
  const zx::vmo& vmo() const { return vmo_; }

  // After Init, this is the File API object with the file's contents.
  const elfldltl::MappedVmoFile& mapped_vmo() const { return mapped_vmo_; }

  // After Init, this has the information relevant for a main executable.
  const ExecInfo& exec_info() const { return exec_info_; }

  // After Init, this is the list of direct DT_NEEDED dependencies in this
  // object.  Each element's .str() / .c_str() pointers point into the mapped
  // file image and are valid for the lifetime of this RemoteDecodedModule (or
  // until it's assigned).
  const NeededList& needed() const { return needed_; }

  // Initialize the module from the provided VMO, representing either the
  // binary or shared library to be loaded.  Create the data structures that
  // make the VMO readable, and scan and decode its phdrs to set and return
  // relevant information about the module to make it ready for relocation and
  // loading.  If the Diagnostics object says to keep going, the module may be
  // uninitialilzed such that HasModule() is false or there is partial
  // information.  This could be used as negative caching for files that have
  // already been examined and found to be invalid.
  template <class Diagnostics>
  bool Init(Diagnostics& diag, size_type page_size) {
    if (auto status = mapped_vmo_.Init(vmo_.borrow()); status.is_error()) {
      // Return true if the Diagnostics object did too, but there is no way to
      // keep going if the file data didn't get mapped in.
      return diag.SystemError("cannot map VMO file", elfldltl::ZirconError{status.status_value()});
    }

    // Get direct pointers to the file header and the program headers inside
    // the mapped file image.
    constexpr elfldltl::NoArrayFromFile<Phdr> kNoPhdrAllocator;
    auto headers = elfldltl::LoadHeadersFromFile<Elf>(diag, mapped_vmo_, kNoPhdrAllocator);
    if (!headers) [[unlikely]] {
      // TODO(mcgrathr): LoadHeadersFromFile doesn't propagate Diagnostics
      // return value on failure.
      return false;
    }

    // Decode phdrs to fill LoadInfo, build ID, etc.
    auto& [ehdr_owner, phdrs_owner] = *headers;
    const Ehdr& ehdr = ehdr_owner;
    const cpp20::span<const Phdr> phdrs = phdrs_owner;
    std::optional<Phdr> relro_phdr;
    std::optional<elfldltl::ElfNote> build_id;
    constexpr elfldltl::NoArrayFromFile<std::byte> kNoBuildIdAllocator;
    auto result = DecodeModulePhdrs(
        diag, phdrs, this->load_info().GetPhdrObserver(page_size),
        elfldltl::PhdrRelroObserver<Elf>(relro_phdr),
        elfldltl::PhdrFileNoteObserver(Elf{}, mapped_vmo_, kNoBuildIdAllocator,
                                       elfldltl::ObserveBuildIdNote(build_id, true)));
    if (!result) [[unlikely]] {
      // DecodeModulePhdrs only fails if Diagnostics said to give up.
      return false;
    }

    auto [dyn_phdr, tls_phdr, stack_size] = *result;

    exec_info_ = {.relative_entry = ehdr.entry, .stack_size = stack_size};

    // After successfully decoding the phdrs, we may now instantiate the module
    // and set its fields.  The symbolizer_modid is not meaningful here.
    this->EmplaceModule(0);

    if (build_id) {
      this->module().build_id = build_id->desc;
    }

    // Apply RELRO protection before segments are aligned & equipped with VMOs.
    if (!this->load_info().ApplyRelro(diag, relro_phdr, page_size, false)) {
      // ApplyRelro only fails if Diagnostics said to give up.
      return false;
    }

    // Fix up segments to be compatible with AlignedRemoteVmarLoader.
    if (!elfldltl::SegmentWithVmo::AlignSegments(diag, this->load_info(), vmo_.borrow(),
                                                 page_size)) {
      // AlignSegments only fails if Diagnostics said to give up.
      return false;
    }

    auto memory = metadata_memory();
    SetModulePhdrs(this->module(), ehdr, this->load_info(), memory);

    // If there was a PT_TLS, fill in tls_module() to be published later.
    // The TLS module ID is not meaningful here, it just has to be nonzero.
    if (tls_phdr) {
      this->SetTls(diag, memory, *tls_phdr, 1);
    }

    // Decode everything else from the PT_DYNAMIC data.  Each DT_NEEDED has an
    // offset into the DT_STRTAB, but the single pass finds DT_STRTAB and sees
    // each DT_NEEDED at the same time.  So the observer just collects their
    // offsets and then those are reified into strings afterwards.
    elfldltl::StdContainer<std::vector>::Container<size_type> needed_offsets;

    if (auto result = DecodeModuleDynamic<Elf>(
            this->module(), diag, memory, dyn_phdr, NeededObserver(needed_offsets),
            elfldltl::DynamicRelocationInfoObserver(this->reloc_info()));
        result.is_error()) [[unlikely]] {
      return result.error_value();
    }

    // Now that DT_STRTAB has been decoded, it's possible to reify each offset
    // into the corresponding SONAME string (and hash it by creating a Soname).
    needed_.reserve(needed_offsets.size());
    for (size_type offset : needed_offsets) {
      std::string_view name = this->symbol_info().string(offset);
      if (name.empty()) [[unlikely]] {
        if (!diag.FormatError("DT_NEEDED has DT_STRTAB offset ", offset, " with DT_STRSZ ",
                              this->symbol_info().strtab().size())) {
          return false;
        }
        continue;
      }
      needed_.emplace_back(name);
    }

    return true;
  }

  // Create and return a memory-adaptor object that serves as a wrapper around
  // this module's LoadInfo and MappedVmoFile.  This is used to translate
  // vaddrs into file-relative offsets in order to read from the VMO.
  MetadataMemory metadata_memory() const {
    return MetadataMemory{
        this->load_info(),
        // The DirectMemory API expects a mutable *this just because it's the
        // API exemplar and toolkit pieces shouldn't presume a Memory API
        // object is usable as const&.  But MappedVmoFile in fact is all const
        // after Init.
        const_cast<elfldltl::MappedVmoFile&>(mapped_vmo_),
    };
  }

 private:
  // This is ultimately just passed to StdContainer<...>::push_back, which
  // never uses it since it will just crash if allocation fails.
  static const constexpr std::string_view kImpossibleError{};

  using NeededObserver = elfldltl::DynamicValueCollectionObserver<
      Elf, elfldltl::ElfDynTag::kNeeded, elfldltl::StdContainer<std::vector>::Container<size_type>,
      kImpossibleError>;

  elfldltl::MappedVmoFile mapped_vmo_;
  NeededList needed_;
  ExecInfo exec_info_;
  zx::vmo vmo_;
};

// RemoteLoadModule is the LoadModule type used in the remote dynamic linker.
// TODO(https://fxbug.dev/326524302): For now it owns the DecodedModule.
// Later it should use a ref-counted smart pointer to const.
template <class Elf>
using RemoteLoadModuleBase = LoadModule<std::unique_ptr<RemoteDecodedModule<Elf>>>;

template <class Elf = elfldltl::Elf<>>
class RemoteLoadModule : public RemoteLoadModuleBase<Elf> {
 public:
  using Base = RemoteLoadModuleBase<Elf>;
  static_assert(std::is_move_constructible_v<Base>);

  using typename Base::Decoded;
  using typename Base::LoadInfo;
  using typename Base::size_type;
  using typename Base::Soname;
  using ExecInfo = typename Decoded::ExecInfo;
  using List = std::vector<RemoteLoadModule>;
  using Loader = elfldltl::AlignedRemoteVmarLoader;

  template <size_t Count>
  using PredecodedPositions = std::array<size_t, Count>;

  // The result returned to the caller after all modules have been decoded.
  template <size_t Count>
  struct DecodeModulesResult {
    List modules;  // The list of all decoded modules.
    size_type max_tls_modid = 0;

    // This corresponds 1:1 to the pre_decoded_modules list passed into
    // DecodeModules, giving the position in .modules where each was moved.
    PredecodedPositions<Count> predecoded_positions;
  };

  RemoteLoadModule() = default;

  RemoteLoadModule(const RemoteLoadModule&) = delete;

  RemoteLoadModule(RemoteLoadModule&&) noexcept = default;

  RemoteLoadModule(const Soname& name, std::optional<uint32_t> loaded_by_modid)
      : Base{name}, loaded_by_modid_{loaded_by_modid} {}

  RemoteLoadModule& operator=(RemoteLoadModule&& other) noexcept = default;

  // Return the index of other module in the list (if any) that requested this
  // one be loaded.  This means that the name() string points into that other
  // module's DT_STRTAB image.
  std::optional<uint32_t> loaded_by_modid() const { return loaded_by_modid_; }

  // Change the module ID (i.e. List index) recording which other module (if
  // any) first requested this module be loaded via DT_NEEDED.  This is
  // normally set in construction at the time of that first request, but for
  // predecoded modules it needs to be updated in place.
  void set_loaded_by_modid(std::optional<uint32_t> loaded_by_modid) {
    loaded_by_modid_ = loaded_by_modid;
  }

  // Decode the main executable VMO and all its dependencies. The `get_dep_vmo`
  // callback is used to retrieve the VMO for each DT_NEEDED entry; it takes a
  // `string_view` and should return a `zx::vmo`.
  template <class Diagnostics, typename GetDepVmo, size_t PredecodedCount>
  static std::optional<DecodeModulesResult<PredecodedCount>> DecodeModules(
      Diagnostics& diag, zx::vmo main_executable_vmo, GetDepVmo&& get_dep_vmo,
      std::array<RemoteLoadModule, PredecodedCount> predecoded_modules) {
    size_type max_tls_modid = 0;

    // Decode the main executable first and save its decoded information to
    // include in the result returned to the caller.
    RemoteLoadModule exec{abi::Abi<>::kExecutableName, std::nullopt};
    if (!exec.Decode(diag, std::move(main_executable_vmo), 0, max_tls_modid)) [[unlikely]] {
      return std::nullopt;
    }

    // The main executable will always be the first entry of the modules list.
    auto [modules, predecoded_positions] =
        DecodeDeps(diag, std::move(exec), std::forward<GetDepVmo>(get_dep_vmo),
                   std::move(predecoded_modules), max_tls_modid);
    if (modules.empty()) [[unlikely]] {
      return std::nullopt;
    }

    return DecodeModulesResult<PredecodedCount>{
        .modules = std::move(modules),
        .max_tls_modid = max_tls_modid,
        .predecoded_positions = predecoded_positions,
    };
  }

  // Initialize the the loader and allocate the address region for the module,
  // updating the module's runtime addr fields on success.
  template <class Diagnostics>
  bool Allocate(Diagnostics& diag, const zx::vmar& vmar) {
    if (this->HasModule()) [[likely]] {
      loader_ = Loader{vmar};
      if (!loader_.Allocate(diag, this->load_info())) {
        return false;
      }
      SetModuleVaddrBounds(this->decoded().module(), this->load_info(), loader_.load_bias());
    }
    return true;
  }

  template <class Diagnostics>
  static bool AllocateModules(Diagnostics& diag, List& modules, zx::unowned_vmar vmar) {
    auto allocate = [&diag, &vmar](auto& module) { return module.Allocate(diag, *vmar); };
    return OnModules(modules, allocate);
  }

  template <class Diagnostics, class ModuleList, typename TlsDescResolver>
  bool Relocate(Diagnostics& diag, ModuleList& modules, const TlsDescResolver& tls_desc_resolver) {
    auto mutable_memory = elfldltl::LoadInfoMutableMemory{
        diag, this->decoded().load_info(),
        elfldltl::SegmentWithVmo::GetMutableMemory<LoadInfo>{this->decoded().vmo().borrow()}};
    if (!mutable_memory.Init()) {
      return false;
    }
    if (!elfldltl::RelocateRelative(diag, mutable_memory, this->reloc_info(), this->load_bias())) {
      return false;
    }
    auto resolver = elfldltl::MakeSymbolResolver(*this, modules, diag, tls_desc_resolver);
    return elfldltl::RelocateSymbolic(mutable_memory, diag, this->reloc_info(), this->symbol_info(),
                                      this->load_bias(), resolver);
  }

  // This returns an OK result only if all modules were fit to be relocated.
  // If decoding failed earlier, this should only be called if Diagnostics said
  // to keep going.  In that case, the error value will be true if every
  // successfully-decoded module's Relocate pass also said to keep going.
  template <class Diagnostics, typename TlsDescResolver>
  static bool RelocateModules(Diagnostics& diag, List& modules,
                              TlsDescResolver&& tls_desc_resolver) {
    auto relocate = [&](auto& module) -> bool {
      if (!module.HasModule()) [[unlikely]] {
        // This module wasn't decoded successfully.  Just skip it.  This
        // doesn't cause a "failure" because the Diagnostics object must have
        // reported the failures in decoding and decided to keep going anyway,
        // so there is nothing new to report.  The caller may have decided to
        // attempt relocation so as to diagnose all its specific errors, rather
        // than bailing out immediately after decoding failed on some of the
        // modules.  Probably callers will more often decide to bail out, since
        // missing dependency modules is an obvious recipe for undefined symbol
        // errors that aren't going to be more enlightening to the user.  But
        // this class supports any policy.
        return true;
      }

      // Resolve against the successfully decoded modules, ignoring the others.
      ld::internal::filter_view valid_modules{
          // The span provides a copyable view of the vector (List), which
          // can't be (and shouldn't be) copied.
          cpp20::span{modules},
          &RemoteLoadModule::HasModule,
      };

      return module.Relocate(diag, valid_modules, tls_desc_resolver);
    };
    return OnModules(modules, relocate);
  }

  // This returns false if any module was not successfully decoded enough to
  // attempt relocation on it.
  static bool AllModulesDecoded(const List& modules) {
    constexpr auto has_module = [](const RemoteLoadModule& module) -> bool {
      return module.HasModule();
    };
    return std::all_of(modules.begin(), modules.end(), has_module);
  }

  // Load the module into its allocated vaddr region.
  template <class Diagnostics>
  bool Load(Diagnostics& diag) {
    return loader_.Load(diag, this->load_info(), this->decoded().vmo().borrow());
  }

  template <class Diagnostics>
  static bool LoadModules(Diagnostics& diag, List& modules) {
    auto load = [&diag](auto& module) { return !module.HasModule() || module.Load(diag); };
    return OnModules(modules, load);
  }

  // This must be the last method called with the loader. Direct the loader to
  // preserve the load image before it is garbage collected.
  void Commit() {
    assert(this->HasModule());
    std::move(loader_).Commit();
  }

  static void CommitModules(List& modules) {
    std::for_each(modules.begin(), modules.end(), [](auto& module) {
      if (module.HasModule()) [[likely]] {
        module.Commit();
      }
    });
  }

  // TODO(https://fxbug.dev/326524302): This will be replaced with separately
  // fetching the RemoteDecodedModule and using it read-only.
  template <class Diagnostics>
  bool Decode(Diagnostics& diag, zx::vmo vmo, uint32_t modid, size_type& max_tls_modid) {
    this->set_decoded(std::make_unique<Decoded>(std::move(vmo)));
    if (!this->decoded().Init(diag, loader_.page_size())) [[unlikely]] {
      return false;
    }

    // Init didn't set link_map.name; it used the generic modid of 0, and the
    // generic TLS module ID of 1 if there was a PT_TLS segment.  Set those for
    // this particular use of the module now.
    this->SetAbiName();
    this->decoded().module().symbolizer_modid = modid;
    this->decoded().module().symbols_visible = true;
    if (this->decoded().tls_module_id() != 0) {
      this->decoded().module().tls_modid = ++max_tls_modid;
    }

    return true;
  }

 private:
  template <typename T>
  static bool OnModules(List& modules, T&& callback) {
    return std::all_of(modules.begin(), modules.end(), std::forward<T>(callback));
  }

  // Decode every transitive dependency module, yielding list in load order
  // starting with the main executable.  On failure this returns an empty List.
  // Otherwise the returned List::front() is always just main_exec moved into
  // place but the list may be longer.  If the Diagnostics object said to keep
  // going after an error, the returned list may be partial and the individual
  // entries may be partially decoded.  They should not be presumed complete,
  // such as calling module(), unless no errors were reported via Diagnostics.
  template <class Diagnostics, typename GetDepVmo, size_t PredecodedCount>
  static std::pair<List, PredecodedPositions<PredecodedCount>> DecodeDeps(
      Diagnostics& diag, RemoteLoadModule main_exec, GetDepVmo&& get_dep_vmo,
      std::array<RemoteLoadModule, PredecodedCount> predecoded_modules, size_type& max_tls_modid) {
    assert(std::all_of(predecoded_modules.begin(), predecoded_modules.end(),
                       [](const auto& m) { return m.HasModule(); }));

    // The list grows with enqueued DT_NEEDED dependencies of earlier elements.
    List modules;

    // This records the position in modules where each predecoded module lands.
    // Initially, each element is -1 to indicate the corresponding argument
    // hasn't been consumed yet.
    constexpr size_t kNpos = -1;
    PredecodedPositions<PredecodedCount> predecoded_positions;
    for (size_t& pos : predecoded_positions) {
      pos = kNpos;
    }

    auto enqueue_deps = [&modules, &predecoded_modules, &predecoded_positions](
                            const std::vector<Soname>& needed,
                            std::optional<uint32_t> loaded_by_modid) {
      // Return true if it's already in the modules list.
      auto in_modules = [&modules](const Soname& soname) -> bool {
        return std::find(modules.begin(), modules.end(), soname) != modules.end();
      };

      // If it's in the predecoded_modules list, then move it to the end of the
      // modules list and update predecoded_positions accordingly.
      auto in_predecoded = [loaded_by_modid, &modules, &predecoded_modules,
                            &predecoded_positions](const Soname& soname) -> bool {
        for (size_t i = 0; i < PredecodedCount; ++i) {
          size_t& pos = predecoded_positions[i];
          RemoteLoadModule& module = predecoded_modules[i];
          if (pos == kNpos && module == soname) {
            pos = modules.size();
            RemoteLoadModule& mod = modules.emplace_back(std::move(module));
            // Record the first module to request this dependency.
            mod.set_loaded_by_modid(loaded_by_modid);
            // Mark that the module is in the symbolic resolution set.
            mod.decoded().module().symbols_visible = true;

            // Use the exact pointer that's the dependent module's DT_NEEDED
            // string for the name field, so remoting can transcribe it.
            mod.set_name(soname);
            mod.set_loaded_by_modid(loaded_by_modid);

            // Assign the module ID that matches the position in the list.
            mod.decoded().module().symbolizer_modid = static_cast<uint32_t>(pos);
            return true;
          }
        }
        return false;
      };

      for (const Soname& soname : needed) {
        if (!in_modules(soname) && !in_predecoded(soname)) {
          modules.emplace_back(soname, loaded_by_modid);
        }
      }
    };

    // Start the list with the main executable, which already has module ID 0.
    // Each module's ID will be the same as its index in the list.
    assert(main_exec.HasModule());
    assert(main_exec.module().symbolizer_modid == 0);
    modules.emplace_back(std::move(main_exec));

    // First enqueue the executable's direct dependencies.
    enqueue_deps(modules.front().decoded().needed(), 0);

    // Now iterate over the queue remaining after the main executable itself,
    // adding indirect dependencies onto the end of the queue until the loop
    // has reached them all.  The total number of iterations is not known until
    // the loop terminates, every transitive dependency having been decoded.
    for (size_t idx = 1; idx < modules.size(); ++idx) {
      RemoteLoadModule& mod = modules[idx];

      // List index becomes symbolizer module ID.
      const uint32_t modid = static_cast<uint32_t>(idx);

      // Only the main executable should already be decoded before this loop,
      // but predecoded modules may have been added to the list during
      // previous iterations.
      if (mod.HasModule()) {
        continue;
      }

      auto vmo = get_dep_vmo(mod.name());
      if (!vmo) [[unlikely]] {
        // If the dep is not found, report the missing dependency, and defer
        // to the diagnostics policy on whether to continue processing.
        if (!diag.MissingDependency(mod.name().str())) {
          return {};
        }
        continue;
      }

      if (!mod.Decode(diag, std::move(vmo), modid, max_tls_modid)) [[unlikely]] {
        return {};
      }

      // If the module wasn't decoded successfully but the Diagnostics object
      // said to keep going, there's no error return.  But there's no list of
      // DT_NEEDED names to enqueue.
      if (mod.HasDecoded()) [[likely]] {
        enqueue_deps(mod.decoded().needed(), modid);
      }
    }

    // Any remaining predecoded modules that weren't reached go on the end of
    // the list, with .symbols_visible=false.
    for (size_t i = 0; i < PredecodedCount; ++i) {
      size_t& pos = predecoded_positions[i];
      RemoteLoadModule& module = predecoded_modules[i];
      if (pos == kNpos) {
        pos = modules.size();
        RemoteLoadModule& mod = modules.emplace_back(std::move(module));
        mod.decoded().module().symbols_visible = false;
        mod.set_name(mod.module().symbols.soname());
        mod.decoded().module().symbolizer_modid = static_cast<uint32_t>(pos);
      }
    }

    return {std::move(modules), std::move(predecoded_positions)};
  }

  Loader loader_;
  std::optional<uint32_t> loaded_by_modid_;
};
static_assert(std::is_move_constructible_v<RemoteLoadModule<>>);

}  // namespace ld

#endif  // LIB_LD_REMOTE_LOAD_MODULE_H_
