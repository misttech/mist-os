// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_STARTUP_LOAD_H_
#define LIB_LD_STARTUP_LOAD_H_

#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/fd.h>
#include <lib/elfldltl/link.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/relro.h>
#include <lib/elfldltl/resolve.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>

#include <algorithm>

#include <fbl/intrusive_double_list.h>

#include "allocator.h"
#include "bootstrap.h"
#include "diagnostics.h"
#include "mutable-abi.h"

namespace ld {

// Usually there are fewer than five segments, so this seems like a reasonable
// upper bound to support.
inline constexpr size_t kMaxSegments = 8;

// There can be quite a few metadata phdrs in addition to a PT_LOAD for each
// segment, so allow a fair few more.
inline constexpr size_t kMaxPhdrs = 32;
static_assert(kMaxPhdrs > kMaxSegments);

// The startup dynamic linker always uses the default ELF layout.
using Elf = elfldltl::Elf<>;
using Addr = typename Elf::Addr;
using Ehdr = typename Elf::Ehdr;
using Phdr = typename Elf::Phdr;
using Sym = typename Elf::Sym;
using Dyn = typename Elf::Dyn;

using RelroBounds = decltype(elfldltl::RelroBounds(Phdr{}, 0));

// StartupLoadModule::Load returns this.
struct StartupLoadResult {
  // This is the number of DT_NEEDED entries seen.  Their strings can't be
  // decoded without a second elfldltl::DecodeDynamic scan since the first one
  // has to find DT_STRTAB and it might not be first.  But the first scan
  // counts how many entries there are, so the second scan can be
  // short-circuited rather than always doing a full O(n) scan of all entries.
  size_t needed_count = 0;

  // These are only of interest for the main executable.
  uintptr_t entry = 0;               // Runtime entry point address.
  std::optional<size_t> stack_size;  // Requested initial stack size.
};

// StartupLoadModule is the LoadModule type used in the startup dynamic linker.
// Its LoadInfo uses fixed storage bounded by kMaxSegments.  The Module is
// allocated separately using the initial-exec allocator.

using StartupLoadModuleBase =
    LoadModule<elfldltl::Elf<>, elfldltl::StaticVector<kMaxSegments>::Container,
               LoadModuleInline::kNo, LoadModuleRelocInfo::kYes>;

template <class Loader>
struct StartupLoadModule : public StartupLoadModuleBase,
                           fbl::DoublyLinkedListable<StartupLoadModule<Loader>*> {
 public:
  using List = fbl::DoublyLinkedList<StartupLoadModule*>;
  using PreloadedModulesList = std::pair<List, cpp20::span<const Dyn>>;

  StartupLoadModule() = delete;

  StartupLoadModule(StartupLoadModule&&) = default;

  template <typename... LoaderArgs>
  explicit StartupLoadModule(const elfldltl::Soname<>& name, LoaderArgs&&... loader_args)
      : StartupLoadModuleBase{name}, loader_{std::forward<LoaderArgs>(loader_args)...} {}

  // This uses the given scratch allocator to create a new module object.
  template <class Allocator, typename... LoaderArgs>
  static StartupLoadModule* New(Diagnostics& diag, Allocator& allocator,
                                const elfldltl::Soname<>& name, LoaderArgs&&... loader_args) {
    fbl::AllocChecker ac;
    StartupLoadModule* module =
        new (allocator, ac) StartupLoadModule{name, std::forward<LoaderArgs>(loader_args)...};
    CheckAlloc(diag, ac, "temporary module data structure");
    return module;
  }

  // Read the file and use Loader::Load on it.  If at least partially
  // successful, this uses the given initial-exec allocator to set up the
  // passive ABI module in this->module().  The allocator only guarantees two
  // mutable allocations at a time, so the caller must then promptly splice it
  // into the link_map list before the next Load call allocates the next one.
  template <class Allocator, class File>
  [[nodiscard]] StartupLoadResult Load(Diagnostics& diag, Allocator& allocator, File&& file,
                                       Elf::size_type& max_tls_modid) {
    // Diagnostics sent to diag during loading will be prefixed with the module
    // name, unless the name is empty as it is for the main executable.
    ModuleDiagnostics module_diag(diag, this->name().str());

    // Read the file header and program headers into stack buffers.
    auto headers = elfldltl::LoadHeadersFromFile<elfldltl::Elf<>>(
        diag, file, elfldltl::FixedArrayFromFile<Phdr, kMaxPhdrs>{});
    if (!headers) [[unlikely]] {
      return {};
    }
    auto& [ehdr_owner, phdrs_owner] = *headers;
    const Ehdr& ehdr = ehdr_owner;
    const cpp20::span<const Phdr> phdrs = phdrs_owner;

    // Decode phdrs to fill LoadInfo and other things.
    std::optional<Phdr> relro_phdr;
    auto [dyn_phdr, tls_phdr, stack_size] =
        DecodeModulePhdrs(diag, phdrs, this->load_info().GetPhdrObserver(loader_.page_size()),
                          elfldltl::PhdrRelroObserver<elfldltl::Elf<>>(relro_phdr));
    set_relro(relro_phdr);

    // load_info() now has enough information to actually load the file.
    if (!loader_.Load(diag, this->load_info(), file.borrow())) [[unlikely]] {
      return {};
    }

    // Now the module's image is in memory!  Allocate the Module object and
    // start filling it in with pointers into the loaded image.

    // Though stored as std::string_view, which isn't in general guaranteed to
    // be NUL-terminated, the name always comes from a DT_NEEDED string in
    // another module's DT_STRTAB, or from an empty string literal.  So in fact
    // it is already NUL-terminated and can be used as a C string for link_map.
    fbl::AllocChecker ac;
    this->NewModule(this->name().c_str(), allocator, ac);
    CheckAlloc(diag, ac, "passive ABI module");

    // All modules allocated by StartupModule are part of the initial exec set
    // and their symbols are inherently visible.
    this->module().symbols_visible = true;

    // This fills in the vaddr bounds and phdrs fields.  Note that module.phdrs
    // might remain empty if the phdrs aren't in the load image, so keep using
    // the stack copy read from the file instead.
    SetModuleLoadInfo(this->module(), ehdr, this->load_info(), loader_.load_bias(), memory());

    // If there was a PT_TLS, fill in tls_module() to be published later.
    if (tls_phdr) {
      SetTls(diag, memory(), ++max_tls_modid, *tls_phdr);
    }

    // A second phdr scan is needed to decode notes now that they can be
    // accessed in memory.
    std::optional<elfldltl::ElfNote> build_id;
    elfldltl::DecodePhdrs(diag, phdrs,
                          elfldltl::PhdrMemoryNoteObserver(elfldltl::Elf<>{}, memory(),
                                                           elfldltl::ObserveBuildIdNote(build_id)));
    if (build_id) {
      this->module().build_id = build_id->desc;
    }

    // Now that there is a Memory object to use, decode the dynamic section.
    size_t needed_count = DecodeDynamic(diag, dyn_phdr);

    // Everything is now prepared to proceed with loading dependencies
    // and performing relocation.
    return {
        .needed_count = needed_count,
        .entry = ehdr.entry + loader_.load_bias(),
        .stack_size = stack_size,
    };
  }

  void set_relro(std::optional<Phdr> relro_phdr) {
    if (relro_phdr) {
      relro_ = elfldltl::RelroBounds(*relro_phdr, loader_.page_size());
    }
  }

  void ProtectRelro(Diagnostics& diag) {
    ModuleDiagnostics module_diag{diag, this->name().str()};
    std::ignore = loader_.ProtectRelro(diag, relro_);
  }

  void Relocate(Diagnostics& diag, const List& modules) {
    ModuleDiagnostics module_diag{diag, this->name().str()};
    elfldltl::RelocateRelative(diag, memory(), reloc_info(), load_bias());
    auto resolver = elfldltl::MakeSymbolResolver(*this, modules, diag);
    elfldltl::RelocateSymbolic(memory(), diag, reloc_info(), symbol_info(), load_bias(), resolver);
  }

  // Returns number of DT_NEEDED entries. See StartupLoadResult, for why that is useful.
  size_t DecodeDynamic(Diagnostics& diag, const std::optional<typename Elf::Phdr>& dyn_phdr) {
    elfldltl::DynamicTagCountObserver<Elf, elfldltl::ElfDynTag::kNeeded> needed;
    // Save the span of Dyn entries for LoadDeps to scan later.
    dynamic_ = DecodeModuleDynamic(module(), diag, memory(), dyn_phdr, needed,
                                   elfldltl::DynamicRelocationInfoObserver(reloc_info()));
    return needed.count();
  }

  // This must be the last method called before the StartupLoadModule is destroyed.
  void Commit() && { std::move(loader_).Commit(); }

  List MakeList() {
    List list;
    list.push_back(this);
    return list;
  }

  Loader& loader() { return loader_; }

  decltype(auto) memory() { return loader_.memory(); }

  template <typename ScratchAllocator, typename InitialExecAllocator, typename GetDepFile,
            typename... LoaderArgs>
  static void LinkModules(Diagnostics& diag, ScratchAllocator& scratch,
                          InitialExecAllocator& initial_exec, StartupLoadModule* main_executable,
                          GetDepFile&& get_dep_file,
                          std::initializer_list<BootstrapModule> preloaded_module_list,
                          size_t executable_needed_count, LoaderArgs&&... loader_args) {
    main_executable->module().symbols_visible = true;

    // The main executable implicitly can use static TLS and doesn't have to
    // have DF_STATIC_TLS set at link time.
    main_executable->module().symbols.set_flags(main_executable->module().symbols.flags() |
                                                elfldltl::ElfDynFlags::kStaticTls);

    List modules = main_executable->MakeList();
    List preloaded_modules =
        MakePreloadedList(diag, scratch, preloaded_module_list, loader_args...);

    // This will be incremented by each Load() of a module that has a PT_TLS.
    Elf::size_type max_tls_modid = main_executable->tls_module_id();

    LoadDeps(diag, scratch, initial_exec, modules, preloaded_modules, executable_needed_count,
             std::forward<GetDepFile>(get_dep_file), max_tls_modid, loader_args...);
    CheckErrors(diag);

    // This assigns static TLS offsets, so it must happen before relocation.
    PopulateAbiTls(diag, initial_exec, modules, max_tls_modid);

    RelocateModules(diag, modules);
    CheckErrors(diag);

    PopulateAbiLoadedModules(modules, std::move(preloaded_modules));

    CommitModules(diag, std::move(modules));
  }

 private:
  void Preload(Module& module, cpp20::span<const Dyn> dynamic) {
    set_module(module);
    dynamic_ = dynamic;
  }

  bool IsLoaded() const { return HasModule(); }

  template <typename Allocator, typename... LoaderArgs>
  static List MakePreloadedList(Diagnostics& diag, Allocator& allocator,
                                std::initializer_list<BootstrapModule> modules,
                                LoaderArgs&&... loader_args) {
    List preloaded_modules;
    for (const auto& [module, dyn] : modules) {
      StartupLoadModule* m = New(diag, allocator, module.soname, loader_args...);
      m->Preload(module, dyn);
      preloaded_modules.push_back(m);
    }
    return preloaded_modules;
  }

  void AddToPassiveAbi(typename List::iterator it, bool symbols_visible) {
    module().symbols_visible = symbols_visible;
    auto& ins_link_map = it->module().link_map;
    auto& this_link_map = module().link_map;
    ins_link_map.next = &this_link_map;
    this_link_map.prev = &ins_link_map;
  }

  // If `soname` is found in `preloaded_modules` it will be removed from that list and pushed into
  // `modules`, making the symbols from those modules visibile to the program.
  static bool FindModule(List& modules, List& preloaded_modules, const elfldltl::Soname<>& soname) {
    if (std::find(modules.begin(), modules.end(), soname) != modules.end()) {
      return true;
    }
    if (auto found = std::find(preloaded_modules.begin(), preloaded_modules.end(), soname);
        found != preloaded_modules.end()) {
      // TODO(fxbug.dev/130483): Mark this preloaded_module as having it's symbols visible to the
      // program.
      modules.push_back(preloaded_modules.erase(found));
      return true;
    }
    return false;
  }

  template <typename Allocator, typename... LoaderArgs>
  void EnqueueDeps(Diagnostics& diag, Allocator& allocator, List& modules, List& preloaded_modules,
                   size_t needed_count, LoaderArgs&&... loader_args) {
    auto handle_needed = [&](std::string_view soname_str) {
      assert(needed_count > 0);
      elfldltl::Soname<> soname{soname_str};
      if (!FindModule(modules, preloaded_modules, soname)) {
        modules.push_back(New(diag, allocator, soname, loader_args...));
      }
      return --needed_count > 0;
    };

    auto observer = elfldltl::DynamicNeededObserver(symbol_info(), handle_needed);
    elfldltl::DecodeDynamic(diag, memory(), dynamic_, observer);
  }

  // `get_dep_file` is called as `std::optional<File>(std::string_view)`.
  // File must meet the requirements of a File type described in
  // lib/elfldltl/memory.h.
  template <typename ScratchAllocator, typename InitialExecAllocator, typename GetDepFile,
            typename... LoaderArgs>
  static void LoadDeps(Diagnostics& diag, ScratchAllocator& scratch,
                       InitialExecAllocator& initial_exec, List& modules, List& preloaded_modules,
                       size_t needed_count, GetDepFile&& get_dep_file,
                       Elf::size_type& max_tls_modid, LoaderArgs&&... loader_args) {
    // Note, this assumes that ModuleList iterators are not invalidated after
    // push_back(), done by `EnqueueDeps`. This is true of lists and
    // StaticVector. No assumptions are made on the validity of the end()
    // iterator, so it is checked at every iteration.
    for (auto it = modules.begin(); it != modules.end(); it++) {
      const bool was_already_loaded = it->IsLoaded();
      if (!was_already_loaded) {
        if (auto file = get_dep_file(it->name())) {
          needed_count = it->Load(diag, initial_exec, *file, max_tls_modid).needed_count;
          assert(it->IsLoaded());
        } else {
          diag.MissingDependency(it->name().str());
          return;
        }
      }
      // The main executable is always first in the list, so its prev is
      // already correct and adding the second module will set its next.
      if (it != modules.begin()) {
        it->AddToPassiveAbi(std::prev(it), true);
        // Referenced preloaded modules can't have DT_NEEDED, do don't bother
        // enqueuing their deps.
        if (was_already_loaded) {
          continue;
        }
      }
      it->EnqueueDeps(diag, scratch, modules, preloaded_modules, needed_count, loader_args...);
    }
  }

  static void RelocateModules(Diagnostics& diag, List& modules) {
    for (auto& module : modules) {
      module.Relocate(diag, modules);

      // TODO: Apply relro
    }
  }

  static void CommitModules(Diagnostics& diag, List modules) {
    while (!modules.is_empty()) {
      auto* module = modules.pop_front();
      std::move(*module).Commit();
      // The `operator delete` this calls does nothing since the scratch
      // allocator doesn't support deallocation per se since the scratch
      // memory will be deallocated en masse, but this calls destructors.
      delete module;
    }
  }

  static void PopulateAbiLoadedModules(List& modules, List preloaded_modules) {
    // We want to add the remaining modules to the list. Their symbols aren't visible for symbolic
    // resolution, but the program can still use their functions even with no relocations resolving
    // to their symbols. Therefore, we need to add these modules to the global module list so they
    // can still be seen by dl_iterate_phdr for unwinding purposes. For example, TLSDESC
    // implementation code lives in the dynamic linker and will be called as part of the TLS
    // implementation without ever having a DT_NEEDED on ld.so. On systems other than Fuchsia it may
    // also be possible to get code from the vDSO without an explicit DT_NEEDED, which is common on
    // Linux.
    auto curr = std::prev(modules.end());
    modules.splice(modules.end(), preloaded_modules);
    for (auto next = std::next(curr), end = modules.end(); next != end; curr = next, next++) {
      next->AddToPassiveAbi(curr, false);
    }

    ld::mutable_abi.loaded_modules = &modules.begin()->module();
  }

  // The passive ABI's TlsModule structs are allocated in a contiguous array
  // indexed by TLS module ID, so they cannot be built up piecemeal in their
  // final locations.  Instead, they're stored directly in the LoadModule when
  // a module has one.  This collects all those and copies them into the
  // passive ABI's array.
  template <typename InitialExecAllocator>
  static void PopulateAbiTls(Diagnostics& diag, InitialExecAllocator& initial_exec_allocator,
                             List& modules, Elf::size_type max_tls_modid) {
    if (max_tls_modid > 0) {
      auto new_array = [&diag, &initial_exec_allocator, max_tls_modid](auto& result) {
        using T = typename std::decay_t<decltype(result.front())>;
        fbl::AllocChecker ac;
        T* array = new (initial_exec_allocator, ac) T[max_tls_modid];
        CheckAlloc(diag, ac, "passive ABI for TLS modules");
        result = {array, max_tls_modid};
      };

      cpp20::span<TlsModule> tls_modules;
      cpp20::span<Addr> tls_offsets;
      new_array(tls_modules);
      new_array(tls_offsets);

      for (StartupLoadModule& module : modules) {
        module.AssignStaticTls(mutable_abi.static_tls_layout, tls_modules, tls_offsets);

#ifdef NDEBUG
        if (module.tls_module_id() == tls_modules.size()) {
          // Don't keep scanning the list if there aren't any more, but skip
          // this optimization if it would prevent a later iteration from
          // hitting an assertion failure if there's a bug that causes an
          // invalid index into the span.
          break;
        }
#endif
      }

      mutable_abi.static_tls_modules = tls_modules;
      mutable_abi.static_tls_offsets = tls_offsets;
    }
  }

  Loader loader_;  // Must be initialized by constructor.
  cpp20::span<const Dyn> dynamic_;
  RelroBounds relro_{};
};

}  // namespace ld

#endif  // LIB_LD_STARTUP_LOAD_H_
