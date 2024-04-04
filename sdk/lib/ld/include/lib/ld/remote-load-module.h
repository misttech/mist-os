// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_REMOTE_LOAD_MODULE_H_
#define LIB_LD_REMOTE_LOAD_MODULE_H_

#include <lib/elfldltl/loadinfo-mutable-memory.h>
#include <lib/elfldltl/resolve.h>
#include <lib/ld/remote-decoded-module.h>

#include <algorithm>
#include <type_traits>
#include <vector>

#include "internal/filter-view.h"

namespace ld {

// RemoteLoadModule is the LoadModule type used in the remote dynamic linker.
// It points to an immutable ld::RemoteDecodedModule describing the ELF file
// itself (see <lib/ld/remote-decoded-module.h>), and then has the other common
// state about the particular instance of the module, such as a name, load
// bias, and relocated data segment contents.
//
// TODO(https://fxbug.dev/326524302): For now it owns the RemoteDecodedModule.
// Later it should use a ref-counted smart pointer to const.

// This the type of the second optional template paraemter to RemoteLoadModule.
// It's a flag saying whether the module is going to be used as a "zygote".  A
// fully-relocated module ready to be loaded as VMOs of relocate data.  In the
// default case, those VMOs are mutable and get directly mapped into a process
// by the Load method, where they may be mutated further via writing mappings.
// In a zygote module, those VMOs are immutable after relocation and instead
// get copy-on-write clones mapped in by Load.
enum class RemoteLoadZygote : bool { kNo = false, kYes = true };

// This is an implementation detail of RemoteLoadModule, below.
template <class Elf>
using RemoteLoadModuleBase = LoadModule<typename RemoteDecodedModule<Elf>::Ptr>;

template <class Elf = elfldltl::Elf<>, RemoteLoadZygote Zygote = RemoteLoadZygote::kNo>
class RemoteLoadModule : public RemoteLoadModuleBase<Elf> {
 public:
  using Base = RemoteLoadModuleBase<Elf>;
  static_assert(std::is_move_constructible_v<Base>);

  // Alias useful types from Decoded and LoadModule.
  using typename Base::Decoded;
  using typename Base::Module;
  using typename Base::size_type;
  using typename Base::Soname;
  using ExecInfo = typename Decoded::ExecInfo;

  // This is the type of the module list.  The ABI remoting scheme relies on
  // this being indexable; see <lib/ld/remote-abi.h> for details.  Being able
  // to use the convenient and efficient indexable containers like std::vector
  // is the main reason RemoteLoadModule needs to be kept movable.
  using List = std::vector<RemoteLoadModule>;

  // RemoteLoadModule has its own LoadInfo that's initially copied from the
  // RemoteDecodedModule, but then gets its own mutable segment VMOs as needed
  // for relocation (or other special-case mutation, as in the ABI remoting).
  //
  // RemoteDecodedModule::LoadInfo always uses elfldltl::SegmentWithVmo::Copy
  // to express that its per-segment VMOs (from partial-page zeroing) should
  // not be mapped writable, only cloned.  However, RemoteLoadModule::LoadInfo
  // can consume its own relocated segments when it's a RemoteLoadZygote::kNo
  // instantiation.  Only the zygote case has reason to keep the segments
  // mutated by relocation immutable thereafter by cloning them for mapping
  // into a process.  (In both cases, the RemoteDecodedModule's segments are
  // left immutable.)
  //
  // Note that the elfldltl::VmarLoader::SegmentVmo partial specializations
  // defined for elfldltl::SegmentWithVmo must exactly match the SegmentWrapper
  // template parameter of the LoadInfo instantiation.  So it's important that
  // the LoadInfo instantiation here uses one of those exactly.  Therefore,
  // this uses a template alias parameterized by the wrapper to do the
  // instantiation inside std::conditional_t directly with the SegmwntWithVmo
  // SegmentWrapper template, rather than a single instantiation with a
  // template alias that uses std::conitional_t inside Segment instantiation.
  // Both ways produce the same Segment types in the LoadInfo, but one makes
  // the partial specializations on elfldltl::VmarLoader::SegmentVmo match.

  template <template <class> class SegmentWrapper>
  using LoadInfoWithWrapper =
      elfldltl::LoadInfo<Elf, RemoteContainer, elfldltl::PhdrLoadPolicy::kBasic, SegmentWrapper>;

  using LoadInfo = std::conditional_t<                      //
      Zygote == RemoteLoadZygote::kYes,                     //
      LoadInfoWithWrapper<elfldltl::SegmentWithVmo::Copy>,  //
      LoadInfoWithWrapper<elfldltl::SegmentWithVmo::NoCopy>>;

  // RemoteDecodedModule uses elfldltl::SegmentWithVmo::AlignSegments, so the
  // loader can rely on just cloning mutable VMOs without partial-page zeroing.
  using Loader = elfldltl::AlignedRemoteVmarLoader;

  // This is the SegmentVmo type that should be used as the basis for the
  // partial specialization of elfldltl::VmarLoader::SegmentVmo, just to make
  // sure it matched the right one.
  using SegmentVmo = std::conditional_t<         //
      Zygote == RemoteLoadZygote::kYes,          //
      elfldltl::SegmentWithVmo::CopySegmentVmo,  //
      elfldltl::SegmentWithVmo::NoCopySegmentVmo>;
  static_assert(std::is_base_of_v<SegmentVmo, Loader::SegmentVmo<LoadInfo>>);

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
      : Base{name}, loaded_by_modid_{loaded_by_modid} {
    static_assert(std::is_move_constructible_v<RemoteLoadModule>);
    static_assert(std::is_move_assignable_v<RemoteLoadModule>);
  }

  RemoteLoadModule& operator=(RemoteLoadModule&& other) noexcept = default;

  // Note this shadows LoadModule::module(), so module() calls in the methods
  // of class and its subclasses return module_ but module() calls in the
  // LoadModule base class return the immutable decoded().module() instead.
  // The only uses LoadModule's own methods make of module() are for the data
  // that is not specific to a particular dynamic linking session: data
  // independent of module name, load bias, and TLS and symbolizer ID numbers.
  const Module& module() const {
    assert(this->HasModule());
    return module_;
  }
  Module& module() {
    assert(this->HasModule());
    return module_;
  }

  // This is set by the Decode method, below.
  size_type tls_module_id() const { return module_.tls_modid; }

  // This is set by the Allocate method, below.
  size_type load_bias() const { return module_.link_map.addr; }

  // This is only set by the Relocate method, below.  Before relocation is
  // complete, consult decoded().load_info() for layout information.  The
  // difference between load_info() and decoded().load_info() is that mutable
  // segment VMOs contain relocated data specific to this RemoteLoadModule
  // where as RemoteDecodedModule only has per-segment VMOs for partial-page
  // zeroing, and those must stay immutable.
  const LoadInfo& load_info() const { return load_info_; }
  LoadInfo& load_info() { return load_info_; }

  constexpr void set_name(const Soname& name) {
    Base::set_name(name);
    SetAbiName();
  }
  constexpr void set_name(std::string_view name) {
    Base::set_name(name);
    SetAbiName();
  }

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
      if (!loader_.Allocate(diag, this->decoded().load_info())) {
        return false;
      }
      SetModuleVaddrBounds(module_, this->decoded().load_info(), loader_.load_bias());
    }
    return true;
  }

  template <class Diagnostics>
  static bool AllocateModules(Diagnostics& diag, List& modules, zx::unowned_vmar vmar) {
    auto allocate = [&diag, &vmar](auto& module) { return module.Allocate(diag, *vmar); };
    return OnModules(modules, allocate);
  }

  // Before relocation can mutate any segments, load_info() needs to be set up
  // with its own copies of the segments, including copy-on-write cloning any
  // per-segment VMOs that decoded() owns.  This can be done earlier if the
  // segments need to be adjusted before relocation.
  template <class Diagnostics>
  bool PrepareLoadInfo(Diagnostics& diag) {
    return !load_info_.segments().empty() ||  // Shouldn't be done twice!
           load_info_.CopyFrom(diag, this->decoded().load_info());
  }

  template <class Diagnostics, class ModuleList, typename TlsDescResolver>
  bool Relocate(Diagnostics& diag, ModuleList& modules, const TlsDescResolver& tls_desc_resolver) {
    if (!PrepareLoadInfo(diag)) [[unlikely]] {
      return false;
    }

    auto mutable_memory = elfldltl::LoadInfoMutableMemory{
        diag, load_info_,
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
    // If any module wasn't decoded successfully, just skip it.  This doesn't
    // cause a "failure" because the Diagnostics object must have reported the
    // failures in decoding and decided to keep going anyway, so there is
    // nothing new to report.  The caller may have decided to attempt
    // relocation so as to diagnose all its specific errors, rather than
    // bailing out immediately after decoding failed on some of the modules.
    // Probably callers will more often decide to bail out, since missing
    // dependency modules is an obvious recipe for undefined symbol errors that
    // aren't going to be more enlightening to the user.  But this class
    // supports any policy.
    ld::internal::filter_view valid_modules{
        // The span provides a copyable view of the vector (List), which
        // can't be (and shouldn't be) copied.
        cpp20::span{modules},
        &RemoteLoadModule::HasModule,
    };
    auto relocate = [&](auto& module) -> bool {
      // Resolve against the successfully decoded modules, ignoring the others.
      return module.Relocate(diag, valid_modules, tls_desc_resolver);
    };
    return std::all_of(valid_modules.begin(), valid_modules.end(), relocate);
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
    return loader_.Load(diag, load_info_, this->decoded().vmo().borrow());
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

    // This returns the Loader::Relro object that holds the VMAR handle.  But
    // we don't need it because the RELRO segment was mapped read-only anyway.
    std::ignore = std::move(loader_).Commit(typename LoadInfo::Region{});
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
    // TODO(https://fxbug.dev/326524302): Creating the RemoteDecodedModule
    // object will move outside this class, and Decode will be replaced with
    // something that just does the rest of this method past this point to
    // attach the pending RemoteLoadModule to the (shareable)
    // RemoteDecodedModule.
    this->set_decoded(Decoded::Create(diag, std::move(vmo), loader_.page_size()));
    if (!this->HasDecoded()) [[unlikely]] {
      return false;
    }

    // Copy the passive ABI Module from the DecodedModule.  That one is the
    // source of truth for all the actual data pointers, but its members
    // related to the vaddr are using the unbiased link-time vaddr range and
    // its module ID indices are not meaningful.  We could store just the or
    // compute the members that vary in each particular dynamic linking session
    // and get the others via indirection through the const decoded() object.
    // But it's simpler just to copy, especially for the ABI remoting logic.
    // It's only a handful of pointers and integers, so it's not a lot to copy.
    module_ = this->decoded().module();

    // The RemoteDecodedModule didn't set link_map.name; it used the generic
    // modid of 0, and the generic TLS module ID of 1 if there was a PT_TLS
    // segment at all.  Set those for this particular use of the module now.
    // The rest will be set later by Allocate via ld::SetModuleVaddrBounds.
    SetAbiName();
    module_.symbolizer_modid = modid;
    if (module_.tls_modid != 0) {
      module_.tls_modid = ++max_tls_modid;
    }

    // This will be reset in DecodeDeps for any pre-decoded modules that aren't
    // referenced.
    module_.symbols_visible = true;

    return true;
  }

 private:
  void SetAbiName() { module_.link_map.name = this->name().c_str(); }

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
            mod.module_.symbols_visible = true;

            // Use the exact pointer that's the dependent module's DT_NEEDED
            // string for the name field, so remoting can transcribe it.
            mod.set_name(soname);
            mod.set_loaded_by_modid(loaded_by_modid);

            // Assign the module ID that matches the position in the list.
            mod.module_.symbolizer_modid = static_cast<uint32_t>(pos);
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
        mod.module_.symbols_visible = false;
        mod.set_name(mod.soname());
        mod.module_.symbolizer_modid = static_cast<uint32_t>(pos);
      }
    }

    return {std::move(modules), std::move(predecoded_positions)};
  }

  Module module_;
  LoadInfo load_info_;
  Loader loader_;
  std::optional<uint32_t> loaded_by_modid_;
};
static_assert(std::is_move_constructible_v<RemoteLoadModule<>>);

}  // namespace ld

#endif  // LIB_LD_REMOTE_LOAD_MODULE_H_
