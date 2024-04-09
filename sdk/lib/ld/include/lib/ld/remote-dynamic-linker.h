// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_REMOTE_DYNAMIC_LINKER_H_
#define LIB_LD_REMOTE_DYNAMIC_LINKER_H_

#include <lib/stdcompat/span.h>
#include <lib/zx/vmar.h>

#include <optional>
#include <type_traits>

#include "abi.h"
#include "remote-abi-stub.h"
#include "remote-abi.h"
#include "remote-load-module.h"

namespace ld {

// ld::RemoteDynamicLinker represents a single remote dynamic linking session.
// It may or may not be the first or only dynamic linking session performed on
// the same process.  Each dynamic linking session defines its own symbolic
// dynamic linking domain and has its own passive ABI (stub dynamic linker).
// TODO(https://fxbug.dev/326524302): Describe Zygote options.
//
// Before creating an ld::RemoteDynamicLinker, the ld::RemoteAbiStub must be
// provided (see <lib/ld/remote-abi-stub.h>).  Only a single ld::RemoteAbiStub
// is needed to reuse the same stub dynamic linker binary across many dynamic
// linking sessions.  The ld::RemoteAbiStub can be provided in a constructor
// argument, or injected with the set_abi_stub method after default
// construction; it must be set before Init is called.
//
// The dynamic linking session proceeds in these phases, with one method each:
//
// * `Init()` starts the session by finding and decoding all the modules.  This
//   starts with initial modules (such as a main executable), and acquires
//   their transitive DT_NEEDED dependencies via a callback function.
//   Additional "pre-decoded" modules may be specified to Init, such as a vDSO:
//   these are linked in even if they are not referenced by any DT_NEEDED
//   dependency; they always appear in the passive ABI, and if unreferenced
//   will be last in the list and have `.symbols_visible = false`.
//
// * `Allocate()` sets the load address for each module using zx_vmar_allocate.
//   Each module gets a VMAR to reserve its part of the address space, and the
//   system call chooses a random available address for it (ASLR).  This is the
//   step that binds the dynamic linking session to a particular address layout
//   and set of relocation results, which depend on all the addresses.  The
//   session is now appropriate only for a single particular process, or a
//   single zygote that will spawn identical processes.
//
// * `Relocate()` fills in all the segment data that will need to be mapped in.
//   That is, it performs relocation on all modules.
//   TODO(https://fxbug.dev/326524302): complete passive ABI in this step
//
// TODO(https://fxbug.dev/326524302): More steps

template <class Elf = elfldltl::Elf<>, RemoteLoadZygote Zygote = RemoteLoadZygote::kNo>
class RemoteDynamicLinker {
 public:
  using AbiStubPtr = typename RemoteAbiStub<Elf>::Ptr;
  using Module = RemoteLoadModule<Elf, Zygote>;
  using DecodedModule = typename Module::Decoded;
  using DecodedModulePtr = typename DecodedModule::Ptr;
  using Soname = typename Module::Soname;
  using List = typename Module::List;
  using size_type = typename Elf::size_type;
  using TlsDescResolver = ld::StaticTlsDescResolver<Elf>;
  using TlsdescRuntimeHooks = typename TlsDescResolver::RuntimeHooks;

  // Each initial module has a name.  For the main executable this is "".
  // For explicitly-loaded modules, it's the name by which they were loaded.
  // This will become Module::name() and link_map.name in the passive ABI.
  struct InitModule {
    Soname name = abi::Abi<Elf>::kExecutableName;
    DecodedModulePtr decoded_module;
  };

  using InitModuleList = cpp20::span<InitModule>;

  // Each predecoded module is only known by its DT_SONAME.  When Init
  // implicitly creates the RemoteLoadModule for it, that will always get a
  // `.name()` string that matches its DT_SONAME, so there is no name supplied
  // here as there is for an initial module.
  //
  // If the module is referenced transitively from an initial module, then its
  // link_map.name pointer will point into the DT_NEEDED string in the
  // DT_STRTAB of the (first) module that used it, which its loaded_by_modid
  // will point to.  If it's not referenced at all, it will go on the list the
  // `symbols_visible` flag in its ld::abi::Abi<>::Module set to false and its
  // link_map.name pointer will point into the DT_SONAME of the DT_STRTAB in
  // this module itself, with std::nullopt for its loaded_by_modid.  Either
  // pointer is an identical string, but the RemoteAbiTranscriber needs to know
  // where each pointer came from.
  struct PredecodedModule {
    DecodedModulePtr decoded_module;
    // TODO(https://fxbug.dev/326524302): Add an optional setting for a module
    // already loaded into the address space.
  };

  template <size_t Count>
  using PredecodedModuleList = std::array<PredecodedModule, Count>;

  // This corresponds 1:1 to the predecoded_modules list passed into
  // Init, giving the position in .modules() where each was inserted.
  template <size_t Count>
  using PredecodedPositions = std::array<size_t, Count>;

  using GetDepResult = std::optional<DecodedModulePtr>;

  RemoteDynamicLinker() = default;

  RemoteDynamicLinker(RemoteDynamicLinker&&) = default;

  explicit RemoteDynamicLinker(AbiStubPtr abi_stub) : abi_stub_{std::move(abi_stub)} {}

  RemoteDynamicLinker& operator=(RemoteDynamicLinker&&) = default;

  const AbiStubPtr& abi_stub() const { return abi_stub_; }

  void set_abi_stub(AbiStubPtr abi_stub) { abi_stub_ = std::move(abi_stub); }

  // Other accessors should be used only after a successful Init call (below).

  RemoteAbi<Elf>& remote_abi() { return remote_abi_; }
  const RemoteAbi<Elf>& remote_abi() const { return remote_abi_; }

  List& modules() { return modules_; }
  const List& modules() const { return modules_; }

  Module& abi_stub_module() { return modules_[stub_modid_]; }
  const Module& abi_stub_module() const { return modules_[stub_modid_]; }

  // When loading a main executable in the normal fashion, it's always the
  // first of the root modules given to Init() and so the first in the
  // modules() list.
  Module& main_module() { return modules_.front(); }
  const Module& main_module() const { return modules_.front(); }

  // Return the runtime address for the main module's Ehdr::e_entry PC address.
  // This should be used after Allocate(), below.
  size_type main_entry() const {
    const Module& main = main_module();
    return main.decoded().exec_info().relative_entry + main.load_bias();
  }

  // Return any PT_GNU_STACK size request from the main module.
  std::optional<size_type> main_stack_size() const {
    return main_module().decoded().exec_info().stack_size;
  }

  // Find an existing Module in the modules() list by name or SONAME.  Returns
  // nullptr if none matches.  The returned pointer is invalidated by adding
  // modules to the list.
  Module* FindModule(const Soname& soname) {
    auto it = std::find(modules_.begin(), modules_.end(), soname);
    if (it != modules_.end()) {
      return &*it;
    }
    return nullptr;
  }

  // This is just a shorthand for std::all_of on the modules() list.
  template <typename T>
  bool OnModules(T&& callback) {
    return OnModules(modules_, std::forward<T>(callback));
  }
  template <typename T>
  bool OnModules(T&& callback) const {
    return OnModules(modules_, std::forward<T>(callback));
  }

  // This returns false if any module was not successfully decoded enough to
  // attempt relocation on it.  If this returns true, some modules may still
  // have errors like missing or incomplete symbol or relocation information,
  // but it's at least valid to call Relocate on them to generate whatever
  // specific errors might result.
  bool AllModulesValid() const { return OnModules(&Module::HasModule); }

  // This returns a view object that iterates over the modules() list but skips
  // modules where the bool(Module&) callback returns false.
  template <typename Predicate>
  auto FilteredModules(Predicate&& predicate) {
    static_assert(std::is_invocable_r_v<bool, Predicate, Module&>);
    return FilteredModules(modules_, std::forward<Predicate>(predicate));
  }
  template <typename Predicate>
  auto FilteredModules(Predicate&& predicate) const {
    static_assert(std::is_invocable_r_v<bool, Predicate, const Module&>);
    return FilteredModules(modules_, std::forward<Predicate>(predicate));
  }

  // This is a shorthand for a view filtered down to modules that have been
  // decoded successfully enough to attempt relocation on them, i.e. where
  // .HasModule() returns true.
  auto ValidModules() { return FilteredModules(&Module::HasModule); }
  auto ValidModules() const { return FilteredModules(&Module::HasModule); }

  // This filters the list with any predicate on Module.
  template <typename T, typename Predicate>
  bool OnFilteredModules(T&& callback, Predicate&& predicate) {
    return OnModules(FilteredModules(std::forward<Predicate>(predicate)),
                     std::forward<T>(callback));
  }
  template <typename T, typename Predicate>
  bool OnFilteredModules(T&& callback, Predicate&& predicate) const {
    return OnModules(FilteredModules(std::forward<Predicate>(predicate)),
                     std::forward<T>(callback));
  }

  // This filters the list to exclude modules where .HasModule() fails.
  template <typename T>
  bool OnValidModules(T&& callback) {
    return OnModules(ValidModules(), std::forward<T>(callback));
  }
  template <typename T>
  bool OnValidModules(T&& callback) const {
    return OnModules(ValidModules(), std::forward<T>(callback));
  }

  // Initialize the session by finding and decoding all the modules.  The
  // initial_modules go on the list first, and then dependencies are added.
  // The predecoded_modules are used by SONAME as needed, and if unreferenced
  // go on the list with `.symbols_visible = false`.  For any other dependency,
  // call the get_dep function as `GepDepResult(Soname)`.  The return type is
  // an alias for `std::optional<DecodedModulePtr>`.  The function is
  // responsible for doing its own diagnostics logging as needed.  If it
  // returns `std::nullopt`, then Init returns `std::nullopt` immediately, as
  // when the Diagnostics object returns false.  If it instead returns a null
  // DecodedModulePtr, that is treated like the Diagnostics object returning
  // true after a failure: that dependency is omitted, but processing
  // continues.  The return value is `std::nullopt` if the Diagnostics object
  // returned false for an error or the get_dep function returned
  // `std::nullopt`; otherwise, it yields array of indices into modules() of
  // where each of the predecoded_modules was placed.  On success, the
  // modules() list is complete, remote_abi() has been initialized, and
  // abi_stub_module() can be used.
  template <class Diagnostics, typename GetDep, size_t PredecodedCount>
  std::optional<PredecodedPositions<PredecodedCount>> Init(
      Diagnostics& diag, InitModuleList initial_modules, GetDep&& get_dep,
      PredecodedModuleList<PredecodedCount> predecoded_modules) {
    static_assert(std::is_invocable_r_v<GetDepResult, GetDep, Soname>);

    assert(abi_stub_);

    assert(!initial_modules.empty());

    assert(std::all_of(predecoded_modules.begin(), predecoded_modules.end(),
                       [](const auto& pdm) { return pdm.decoded_module->HasModule(); }));

    // Start the list with the initial modules.  The first one is the main
    // executable if there is such a thing.  It gets symbolizer module ID 0.
    for (auto& mod : initial_modules) {
      EmplaceModule(mod.name, std::nullopt, std::move(mod.decoded_module));
    }

    // This records the position in modules_ where each predecoded module
    // lands.  Initially, each element is -1 to indicate the corresponding
    // argument hasn't been consumed yet.
    constexpr size_t kNpos = -1;
    PredecodedPositions<PredecodedCount> predecoded_positions;
    for (size_t& pos : predecoded_positions) {
      pos = kNpos;
    }

    // If it's in the predecoded_modules list, then return that decoded module
    // and update predecoded_positions accordingly.
    auto find_predecoded = [&predecoded_modules, &predecoded_positions](
                               const Soname& soname, uint32_t modid) -> DecodedModulePtr {
      for (size_t i = 0; i < PredecodedCount; ++i) {
        size_t& pos = predecoded_positions[i];
        PredecodedModule& predecoded = predecoded_modules[i];
        if (pos != kNpos) {
          // This one was already been used.
          assert(!predecoded.decoded_module);
          continue;
        }
        if (predecoded.decoded_module->soname() == soname) {
          predecoded_positions[i] = modid;
          return std::exchange(predecoded.decoded_module, {});
        }
      }
      return {};
    };

    // The initial modules now form a queue of modules to be loaded.  Iterate
    // over that queue, adding additional entries onto the queue for each
    // DT_NEEDED list.  Once past the initial modules, each RemoteDecodedModule
    // must be acquired.  The total number of iterations is not known until the
    // loop terminates, every transitive dependency having been decoded.
    for (size_t idx = 0; idx < modules_.size(); ++idx) {
      Module& mod = modules_[idx];

      // List index becomes symbolizer module ID.
      const uint32_t modid = static_cast<uint32_t>(idx);

      if (!mod.HasDecoded()) {
        // This isn't one of the initial modules, so it's only a needed SONAME.
        if (mod.name() == kStubSoname) {
          // The stub dynamic linker is a predecoded module that's handled
          // specially.
          stub_modid_ = modid;
          mod.set_decoded(abi_stub_->decoded_module(), modid, true, max_tls_modid_);
        } else if (auto predecoded = find_predecoded(mod.name(), modid)) {
          // The SONAME matches one of the predecoded modules.
          mod.set_decoded(std::move(predecoded), modid, true, max_tls_modid_);
        } else {
          // Use the callback to get a DecodedModulePtr for the SONAME.
          GetDepResult result = get_dep(mod.name());
          if (!result) [[unlikely]] {
            return {};
          }
          if (!*result) [[unlikely]] {
            // The get_dep function failed, but said to keep going anyway.
            continue;
          }
          mod.set_decoded(std::move(*result), modid, true, max_tls_modid_);
        }
      }

      // This extends modules_ with new DT_NEEDED modules.
      EnqueueDeps(mod);
    }

    // Any remaining predecoded modules that weren't reached go on the end of
    // the list, with .symbols_visible=false.
    for (size_t i = 0; i < PredecodedCount; ++i) {
      size_t& pos = predecoded_positions[i];
      if (pos != kNpos) {
        // This one was already placed.
        assert(!predecoded_modules[i].decoded_module);
        continue;
      }
      DecodedModulePtr& decoded = predecoded_modules[i].decoded_module;
      assert(decoded);
      EmplaceModule(decoded->soname(), std::nullopt, std::move(decoded), false);
    }

    // And finally the same for the stub dynamic linker.
    if (stub_modid_ == 0) {
      stub_modid_ = static_cast<uint32_t>(modules_.size());
      DecodedModulePtr decoded = abi_stub_->decoded_module();
      EmplaceModule(kStubSoname, std::nullopt, std::move(decoded), false);
    }

    Module& stub_module = modules_[stub_modid_];
    zx::result abi_result =
        remote_abi_.Init(diag, abi_stub_, stub_module, modules_, max_tls_modid_);
    if (abi_result.is_error() &&
        !diag.SystemError("cannot initialize remote ABI heap",
                          elfldltl::ZirconError{abi_result.error_value()})) {
      return {};
    }

    return predecoded_positions;
  }

  // Initialize the loader and allocate the address region for each module,
  // updating their runtime addr fields on success.  This must be called before
  // Relocate or Load.  If Init was told to keep going after decoding errors,
  // then this will just skip any modules that weren't substantially decoded.
  template <class Diagnostics>
  bool Allocate(Diagnostics& diag, zx::unowned_vmar vmar) {
    auto allocate = [&diag, &vmar = *vmar](Module& module) -> bool {
      return module.Allocate(diag, vmar);
    };
    return OnModules(ValidModules(), allocate);
  }

  // Acquire a StaticTlsDescResolver for Relocate that uses the stub dynamic
  // linker's TLSDESC entry points.  This resolver handles undefined weak
  // resolutions and it handles symbols resolved to a definition in a module
  // using static_tls_bias().  This is what's used by default if Relocate is
  // called with one argument.
  //
  // This can only be used after Allocate(), as that determines the runtime
  // code addresses for the stub dynamic linker; these addresses are stored in
  // the returned object.  Note they can also be modified later with e.g.
  // `.SetHook(TlsdescRuntime::kStatic, custom_hook)`; see <lib/ld/tlsdesc.h>.
  TlsDescResolver tls_desc_resolver() const {
    return abi_stub_->tls_desc_resolver(abi_stub_module().load_bias());
  }

  // Shorthand for the two-argument Relocate method below.
  template <class Diagnostics>
  bool Relocate(Diagnostics& diag) {
    return Relocate(diag, tls_desc_resolver());
  }

  // Perform relocations on all modules.  The modules() list gives the set and
  // order of modules used for symbol resolution.
  //
  // For dynamic TLS references, the tls_desc_resolver is a callable object
  // with the signatures of ld::StaticTlsDescResolver (see <lib/ld/tlsdesc.h>),
  // usually from the tls_desc_resolver() method above to use runtime callbacks
  // supplied in the stub dynamic linker.
  //
  // If any module was not successfully decoded sufficiently to call the
  // Relocate method on that ld::RemoteLoadModule, then that module is just
  // skipped.  This doesn't cause a "failure" here because the Diagnostics
  // object must have reported the failures in decoding and decided to keep
  // going anyway, so there is nothing new to report.  The caller may have
  // decided to attempt relocation so as to diagnose all its specific errors,
  // rather than bailing out immediately after decoding failed on some of the
  // modules.  Probably callers will more often decide to bail out, since
  // missing dependency modules is an obvious recipe for undefined symbol
  // errors that aren't going to be more enlightening to the user.  But this
  // class supports any policy.
  template <class Diagnostics, typename TlsDescResolverType>
  bool Relocate(Diagnostics& diag, TlsDescResolverType&& tls_desc_resolver) {
    // If any module wasn't decoded successfully, just skip it.
    auto valid_modules = ValidModules();
    auto relocate = [&](auto& module) -> bool {
      // Resolve against the successfully decoded modules, ignoring the others.
      return module.Relocate(diag, valid_modules, tls_desc_resolver);
    };
    return OnModules(valid_modules, relocate);
  }

 private:
  static constexpr Soname kStubSoname = abi::Abi<Elf>::kSoname;

  // Add a new module to the list.  If no decoded_module is supplied here,
  // it must be fetched later as a dependency.
  void EmplaceModule(const Soname& name, std::optional<uint32_t> loaded_by_modid,
                     DecodedModulePtr decoded_module = {}, bool symbols_visible = true) {
    modules_.emplace_back(name, loaded_by_modid);
    if (decoded_module) {
      const uint32_t modid = static_cast<uint32_t>(modules_.size() - 1);
      modules_.back().set_decoded(std::move(decoded_module), modid, symbols_visible,
                                  max_tls_modid_);
    }
  }

  // Call EmplaceModule for each DT_NEEDED that's not already on the list.
  void EnqueueDeps(const Module& module) {
    if (!module.HasModule()) [[unlikely]] {
      // The module wasn't decoded properly so its DT_NEEDED was never
      // extracted, but Diagnostics said to keep going.
      assert(module.decoded().needed().empty());
      return;
    }

    const uint32_t loaded_by_modid = module.module().symbolizer_modid;
    for (const Soname& soname : module.decoded().needed()) {
      if (!FindModule(soname)) {
        EmplaceModule(soname, loaded_by_modid);
      }
    }
  }

  template <typename List, typename T>
  static bool OnModules(List&& modules, T&& callback) {
    auto invoke = [&callback](auto& module) -> bool {
      // Wrap it in std::invoke so a pointer to method can be used, since
      // std::all_of doesn't.  (C++20 std::ranges::all_of does, so that could
      // replace OnModules completely.)
      return std::invoke(callback, module);
    };
    return std::all_of(std::begin(modules), std::end(modules), invoke);
  }

  template <typename Predicate>
  static auto FilteredModules(List& modules, Predicate&& predicate) {
    // Use a span to get a copyable view of the List, which must be used only
    // by reference.
    cpp20::span view{modules};
    return ld::internal::filter_view{view, std::forward<Predicate>(predicate)};
  }

  AbiStubPtr abi_stub_;
  RemoteAbi<Elf> remote_abi_;
  List modules_;
  size_type max_tls_modid_ = 0;
  uint32_t stub_modid_ = 0;
};

}  // namespace ld

#endif  // LIB_LD_REMOTE_DYNAMIC_LINKER_H_
