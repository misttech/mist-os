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
  StartupLoadResult Load(Diagnostics& diag, Allocator& allocator, File&& file) {
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
    auto [dyn_phdr, stack_size] =
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

    // This fills in the vaddr bounds and phdrs fields.  Note that module.phdrs
    // might remain empty if the phdrs aren't in the load image, so keep using
    // the stack copy read from the file instead.
    SetModuleLoadInfo(this->module(), ehdr, this->load_info(), loader_.load_bias(), memory());

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
    auto resolver = elfldltl::MakeSymbolResolver(symbol_info(), modules, diag);
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
    List modules = main_executable->MakeList();
    List preloaded_modules =
        MakePreloadedList(diag, scratch, preloaded_module_list, loader_args...);

    LoadDeps(diag, scratch, initial_exec, modules, preloaded_modules, executable_needed_count,
             std::forward<GetDepFile>(get_dep_file), loader_args...);
    CheckErrors(diag);

    RelocateModules(diag, modules);
    CheckErrors(diag);

    PopulateLdAbi(modules, std::move(preloaded_modules));

    CommitModules(diag, std::move(modules));
  }

 private:
  void Preload(Module& module, cpp20::span<const Dyn> dynamic) {
    set_module(module);
    dynamic_ = dynamic;
  }

  bool IsLoaded() const { return module_; }

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

  void InsertModuleInLinkMap(typename List::iterator it) {
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
      elfldltl::Soname<> soname{soname_str};
      if (!FindModule(modules, preloaded_modules, soname)) {
        modules.push_back(New(diag, allocator, soname, loader_args...));
      }
      return --needed_count > 0;
    };

    auto observer = elfldltl::DynamicNeededObserver(symbol_info(), handle_needed);
    elfldltl::DecodeDynamic(diag, memory(), dynamic_, observer);
  }

  // `get_dep_file` takes a `string_view` and should return an `std::optional<File>`. `File`
  // must meet the requirements of a File type described in lib/elfldltl/memory.h.
  template <typename ScratchAllocator, typename InitialExecAllocator, typename GetDepFile,
            typename... LoaderArgs>
  static void LoadDeps(Diagnostics& diag, ScratchAllocator& scratch,
                       InitialExecAllocator& initial_exec, List& modules, List& preloaded_modules,
                       size_t needed_count, GetDepFile&& get_dep_file,
                       LoaderArgs&&... loader_args) {
    // Note, this assumes that ModuleList iterators are not invalidated after push_back(), done
    // by `EnqueueDeps`. This is true of lists and StaticVector. No assumptions are made on the
    // validity of the end() iterator, so it is checked at every iteration.
    for (auto it = modules.begin(); it != modules.end(); it++) {
      const bool was_already_loaded = it->IsLoaded();
      if (!was_already_loaded) {
        if (auto file = get_dep_file(it->name())) {
          it->Load(diag, initial_exec, *file);
          assert(it->IsLoaded());
        } else {
          diag.MissingDependency(it->name().str());
        }
      }
      if (it != modules.begin()) {
        it->InsertModuleInLinkMap(std::prev(it));
        // Referenced preloaded modules can't have DT_NEEDED.
        // The third case is the main executable, which is already loaded but can;
        // it's always the first module in the list.
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
      delete module;
    }
  }

  static void PopulateLdAbi(List& modules, List preloaded_modules) {
    // TODO: In the future we will add these modules to the list, but without their symbols
    // visible. For now, we just unconditionally put the remaining preloaded_modules in the
    // list.
    auto curr = std::prev(modules.end());
    modules.splice(modules.end(), preloaded_modules);
    for (auto next = std::next(curr), end = modules.end(); next != end; curr = next, next++) {
      next->InsertModuleInLinkMap(curr);
    }

    ld::mutable_abi.loaded_modules = &modules.begin()->module();
  }

  Loader loader_;  // Must be initialized by constructor.
  cpp20::span<const Dyn> dynamic_;
  RelroBounds relro_{};
};

}  // namespace ld

#endif  // LIB_LD_STARTUP_LOAD_H_
