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
