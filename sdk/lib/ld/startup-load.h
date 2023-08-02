// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_STARTUP_LOAD_H_
#define LIB_LD_STARTUP_LOAD_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/relro.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>

#include <fbl/intrusive_double_list.h>

#include "allocator.h"
#include "diagnostics.h"

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
  StartupLoadModule() = delete;

  StartupLoadModule(StartupLoadModule&&) = default;

  template <typename... LoaderArgs>
  explicit StartupLoadModule(std::string_view name, LoaderArgs&&... loader_args)
      : StartupLoadModuleBase{name}, loader_{std::forward<LoaderArgs>(loader_args)...} {}

  // This uses the given scratch allocator to create a new module object.
  template <class Allocator, typename... LoaderArgs>
  static StartupLoadModule* New(Diagnostics& diag, Allocator& allocator, std::string_view name,
                                LoaderArgs&&... loader_args) {
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

    fbl::AllocChecker ac;
    this->NewModule(allocator, ac);
    CheckAlloc(diag, ac, "passive ABI module");

    // Though stored as std::string_view, which isn't in general guaranteed to
    // be NUL-terminated, the name always comes from a DT_NEEDED string in
    // another module's DT_STRTAB, or from an empty string literal.  So in fact
    // it is already NUL-terminated and can be used as a C string for link_map.
    this->module().link_map.name = this->name().c_str();

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
    elfldltl::DynamicTagCountObserver<Elf, elfldltl::ElfDynTag::kNeeded> needed;
    DecodeModuleDynamic(this->module(), diag, memory(), dyn_phdr, needed,
                        elfldltl::DynamicRelocationInfoObserver(this->reloc_info()));

    // Everything is now prepared to proceed with loading dependencies
    // and performing relocation.
    return {
        .needed_count = needed.count(),
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

  Loader& loader() { return loader_; }

  decltype(auto) memory() { return loader_.memory(); }

 private:
  Loader loader_;  // Must be initialized by constructor.
  RelroBounds relro_{};
};

}  // namespace ld

#endif  // LIB_LD_STARTUP_LOAD_H_
