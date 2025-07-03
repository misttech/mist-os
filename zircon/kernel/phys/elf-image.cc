// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/elf-image.h"

#include <inttypes.h>
#include <lib/arch/cache.h>
#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/dynamic.h>
#include <lib/elfldltl/link.h>
#include <lib/fit/defer.h>
#include <lib/zbitl/error-stdio.h>
#include <string.h>
#include <zircon/assert.h>
#include <zircon/limits.h>

#include <ktl/atomic.h>
#include <ktl/span.h>
#include <ktl/utility.h>
#include <ktl/variant.h>
#include <phys/allocation.h>
#include <phys/symbolize.h>
#include <phys/zircon-info-note.h>

#include <ktl/enforce.h>

namespace {

constexpr ktl::string_view kDiagnosticsPrefix = "Cannot load ELF image: ";

auto PanicDiagnostics() {
  return elfldltl::Diagnostics{elfldltl::PrintfDiagnosticsReport(__zx_panic, kDiagnosticsPrefix),
                               elfldltl::DiagnosticsPanicFlags()};
}

// TODO(mcgrathr): BFD ld produces a spurious empty .eh_frame with its own
// empty PT_LOAD segment. This is harmless enough to the actual layout,
// but triggers a FormatWarning.
#if 1  // def __clang__
auto GetDiagnostics() { return PanicDiagnostics(); }
#else
constexpr auto kPanicReport = elfldltl::PrintfDiagnosticsReport(__zx_panic, kDiagnosticsPrefix);
using DiagBase = elfldltl::Diagnostics<decltype(kPanicReport), elfldltl::DiagnosticsPanicFlags>;
struct NoWarnings : public DiagBase {
  constexpr NoWarnings() : DiagBase(kPanicReport) {}
  static constexpr auto FormatWarning = [](auto&&...) { return true; };
};
auto GetDiagnostics() { return NoWarnings(); }
#endif

}  // namespace

fit::result<ElfImage::Error> ElfImage::Init(ElfImage::BootfsDir dir, ktl::string_view name,
                                            bool relocated) {
  if (auto found = dir.find(name); found != dir.end()) {
    // Singleton ELF file, no patches.
    dir.ignore_error();
    auto result = InitFromFile(found, relocated);
    package_ = dir.directory();
    name_ = name;
    return result;
  }

  auto subdir = dir.subdir(name);
  if (subdir.is_error()) {
    return subdir.take_error();
  }
  return InitFromDir(*subdir, name, relocated);
}

fit::result<ElfImage::Error> ElfImage::InitFromDir(ElfImage::BootfsDir subdir,
                                                   ktl::string_view name, bool relocated) {
  // Find the ELF file in the directory.
  auto it = subdir.find(kImageName);
  if (it == subdir.end()) {
    if (auto result = subdir.take_error(); result.is_error()) {
      return result.take_error();
    }
    return fit::error{Error{
        .reason = "ELF file not found in image directory"sv,
        .filename = kImageName,
    }};
  }
  subdir.ignore_error();

  // Now find the code patches.
  if (auto result = patcher_.Init(subdir); result.is_error()) {
    return result.take_error();
  }

  auto result = InitFromFile(it, relocated);
  name_ = name;
  return result;
}

fit::result<ElfImage::Error> ElfImage::InitFromFile(ElfImage::BootfsDir::iterator file,
                                                    bool relocated) {
  package_ = file.view().directory();
  name_ = file->name;
  image_.set_image(file->data);

  auto diagnostics = GetDiagnostics();
  auto phdr_allocator = elfldltl::NoArrayFromFile<Elf::Phdr>();
  auto headers = elfldltl::LoadHeadersFromFile<Elf>(diagnostics, image_, phdr_allocator);
  auto [ehdr, phdrs] = *headers;

  auto observe_zircon_info_note =
      [this](const elfldltl::ElfNote& note) -> fit::result<fit::failed, bool> {
    if (note.Is(kZirconInfoNoteName)) {
      ZX_ASSERT_MSG(!zircon_info_,
                    "second ZirconInfo note (type %#" PRIx32
                    ", desc %zu bytes after existing type %#" PRIx32
                    ", desc %zu bytes) in ELF image %.*s",
                    note.type, note.desc.size_bytes(), zircon_info_->type,
                    zircon_info_->desc.size_bytes(), static_cast<int>(name_.size()), name_.data());
      zircon_info_ = note;
    }
    return fit::ok(true);
  };
  ktl::optional<Elf::Phdr> relro, dynamic, interp;
  elfldltl::DecodePhdrs(  //
      diagnostics, phdrs, load_info_.GetPhdrObserver(ZX_PAGE_SIZE),
      elfldltl::PhdrFileNoteObserver(  //
          Elf(), image_, elfldltl::NoArrayFromFile<ktl::byte>(),
          elfldltl::ObserveBuildIdNote(build_id_), observe_zircon_info_note),
      elfldltl::PhdrRelroObserver<Elf>(relro), elfldltl::PhdrDynamicObserver<Elf>(dynamic),
      elfldltl::PhdrStackObserver<Elf>(stack_size_), elfldltl::PhdrInterpObserver<Elf>(interp));

  image_.set_base(load_info_.vaddr_start());
  entry_ = ehdr.entry;

  if (relocated) {
    // In the phys context, all the relocations are done in place before the
    // image is considered "loaded".  Update the load segments to indicate
    // RELRO protections have already been applied.
    load_info_.ApplyRelro(diagnostics, relro, ZX_PAGE_SIZE, true);
  }

  if (dynamic) {
    dynamic_ = *image_.ReadArray<Elf::Dyn>(dynamic->offset, dynamic->filesz / sizeof(Elf::Dyn));
  }

  if (interp) {
    auto chars = image_.ReadArrayFromFile<char>(interp->offset, elfldltl::NoArrayFromFile<char>(),
                                                interp->filesz);
    ZX_ASSERT_MSG(chars, "PT_INTERP has invalid offset range [%#" PRIxPTR ", %#" PRIxPTR ")",
                  static_cast<uintptr_t>(interp->offset),
                  static_cast<uintptr_t>(interp->offset + interp->filesz));
    ZX_ASSERT_MSG(!chars->empty(), "PT_INTERP has zero filesz");
    ZX_ASSERT_MSG(chars->back() == '\0', "PT_INTERP missing NUL terminator");
    interp_.emplace(chars->data(), chars->size() - 1);
  }

  return fit::ok();
}

ktl::span<ktl::byte> ElfImage::GetBytesToPatch(const code_patching::Directive& patch) {
  ktl::span<ktl::byte> file = image_.image();
  ZX_ASSERT_MSG(patch.range_start >= image_.base() && file.size() >= patch.range_size &&
                    file.size() - patch.range_size >= patch.range_start - image_.base(),
                "Patch ID %#" PRIx32 " range [%#" PRIx64 ", %#" PRIx64
                ") is outside file bounds [%#" PRIxPTR ", %#" PRIxPTR ")",
                patch.id, patch.range_start, patch.range_start + patch.range_size, image_.base(),
                image_.base() + file.size());
  return file.subspan(static_cast<size_t>(patch.range_start - image_.base()), patch.range_size);
}

Allocation ElfImage::Load(memalloc::Type type, ktl::optional<uint64_t> relocation_address,
                          bool in_place_ok) {
  auto endof = [](const auto& last) { return last.offset() + last.filesz(); };
  const uint64_t load_size = ktl::visit(endof, load_info_.segments().back());

  auto update_load_address_and_symbolize = fit::defer([relocation_address, this]() {
    // Update the load address before having emitting otherwise-misleading
    // markup.
    set_load_address(relocation_address.value_or(physical_load_address()));
    gSymbolize->OnLoad(*this);
  });

  if (in_place_ok && CanLoadInPlace()) {
    // TODO(https://fxbug.dev/42065186): Could have a memalloc::Pool feature to
    // reclassify the memory range to the new type.

    // The full vaddr_size() fits in the pages the BOOTFS file occupies.  If
    // there is any bss (memsz > filesz), it may overlap with some nonzero file
    // contents and not just the BOOTFS page-alignment padding.  Zero it all.
    ZX_DEBUG_ASSERT(ZBI_BOOTFS_PAGE_ALIGN(image_.image().size_bytes()) >= load_info_.vaddr_size());
    memset(image_.image().data() + load_size, 0,
           static_cast<size_t>(load_info_.vaddr_size() - load_size));
    return {};
  }

  fbl::AllocChecker ac;
  Allocation image = Allocation::New(ac, type, load_info_.vaddr_size(), ZX_PAGE_SIZE);
  if (!ac.check()) {
    ZX_PANIC("cannot allocate phys ELF load image of %#zx bytes",
             static_cast<size_t>(load_info_.vaddr_size()));
  }

  ZX_ASSERT_MSG(load_size <= image.size_bytes(), "load_size %#" PRIx64 " > allocation size %#zx",
                load_size, image.size_bytes());

  // Copy the full load image into the new allocation.  The load_size is
  // page-rounded and thus can go past the formal end of the file, indicating
  // how mapping from a file would work.  To get the equivalent effect, just
  // fill the remainder of the allocated page with zero while zeroing any
  // following bss (memsz > filesz).
  const size_t copy = ktl::min(static_cast<size_t>(load_size), image_.image().size_bytes());
  memcpy(image.get(), image_.image().data(), copy);
  memset(image.get() + copy, 0, load_info_.vaddr_size() - copy);

  // Hereafter image_ refers to the now-loaded image, not the original file.
  image_.set_image(image.data());

  // Ensure that by the time we return, it's safe to jump into this code.
  // Later relocation won't touch executable segments, so additional
  // synchronization should not be required unless the image is copied
  // elsewhere in physical memory.
  ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
  arch::GlobalCacheConsistencyContext cache;
  cache.SyncRange(physical_load_address(), load_info_.vaddr_size());

  return image;
}

void ElfImage::Relocate() {
  ZX_DEBUG_ASSERT(load_bias_);  // The load address has already been chosen.
  if (!dynamic_.empty()) {
    auto diagnostics = GetDiagnostics();
    elfldltl::RelocationInfo<Elf> reloc_info;
    elfldltl::DecodeDynamic(diagnostics, image_, dynamic_,
                            elfldltl::DynamicRelocationInfoObserver(reloc_info));
    ZX_ASSERT(reloc_info.rel_symbolic().empty());
    ZX_ASSERT(reloc_info.rela_symbolic().empty());
    bool relocated = elfldltl::RelocateRelative(diagnostics, image_, reloc_info, *load_bias_);
    ZX_ASSERT(relocated);

    // Make sure everything is written before the image is used as code.
    ktl::atomic_signal_fence(ktl::memory_order_seq_cst);
  }
}

void ElfImage::AssertInterpMatchesBuildId(ktl::string_view prefix,
                                          const elfldltl::ElfNote& build_id_note) {
  ZX_DEBUG_ASSERT(build_id_note.IsBuildId());
  ZX_ASSERT_MSG(build_id_note.desc.size() <= kMaxBuildIdLen,
                "%.*s: reference build ID of %zu bytes > max supported %zu",
                static_cast<int>(prefix.size()), prefix.data(),  //
                build_id_note.desc.size(), kMaxBuildIdLen);
  char build_id_hex_buffer[kMaxBuildIdLen * 2];
  ktl::string_view build_id_hex = build_id_note.HexString(build_id_hex_buffer);
  ZX_ASSERT_MSG(interp_, "%.*s: ELF image has no PT_INTERP (expected %.*s)",
                static_cast<int>(prefix.size()), prefix.data(),
                static_cast<int>(build_id_hex.size()), build_id_hex.data());
  ZX_ASSERT_MSG(*interp_ == build_id_hex,
                "%.*s: ELF image PT_INTERP (size %zu) %.*s != expected (size %zu) %.*s",
                static_cast<int>(prefix.size()), prefix.data(),      //
                interp_->size(),                                     //
                static_cast<int>(interp_->size()), interp_->data(),  //
                build_id_hex.size(),                                 //
                static_cast<int>(build_id_hex.size()), build_id_hex.data());
}

void ElfImage::InitSelf(ktl::string_view name, elfldltl::DirectMemory& memory, uintptr_t load_bias,
                        const Elf::Phdr& load_segment, ktl::span<const ktl::byte> build_id_note) {
  image_.set_image(memory.image());
  image_.set_base(memory.base());
  load_bias_ = load_bias;

  auto diag = PanicDiagnostics();
  ZX_ASSERT(load_info_.AddSegment(diag, ZX_PAGE_SIZE, load_segment));

  elfldltl::ElfNoteSegment<> notes(build_id_note);
  ZX_DEBUG_ASSERT(notes.begin() != notes.end());
  ZX_DEBUG_ASSERT(++notes.begin() == notes.end());
  build_id_ = *notes.begin();

  name_ = name;

  OnHandoff();
}

void ElfImage::PrintPatch(const code_patching::Directive& patch,
                          ktl::initializer_list<ktl::string_view> strings) const {
  printf("%s: code-patching on %.*s: ", gSymbolize->name(), static_cast<int>(name_.size()),
         name_.data());
  for (ktl::string_view str : strings) {
    stdout->Write(str);
  }
  printf(": [%#" PRIx64 ", %#" PRIx64 ")\n", patch.range_start,
         patch.range_start + patch.range_size);
}

fit::result<ElfImage::Error> ElfImage::SeparateZeroFill() {
  for (auto it = load_info_.segments().begin(); it != load_info_.segments().end(); ++it) {
    auto* segment = ktl::get_if<LoadInfo::DataWithZeroFillSegment>(&*it);
    if (!segment) {  // Not a DataWithZeroFillSegment.
      continue;
    }

    size_t partial_page = segment->filesz() % ZX_PAGE_SIZE;
    if (partial_page > 0) {
      // Zero the partial page that is part of the zero-fill area but will
      // remain in the original segment transformed into plain DataSegment.
      partial_page = ZX_PAGE_SIZE - partial_page;
      size_t fill_start = segment->offset() + segment->filesz();
      ktl::span fill_bytes = image_.image().subspan(fill_start, partial_page);
      memset(fill_bytes.data(), 0, fill_bytes.size_bytes());
    }

    size_t new_filesz = segment->filesz() + partial_page;
    size_t zero_fill_vaddr = segment->vaddr() + new_filesz;
    size_t zero_fill_size = segment->memsz() - new_filesz;

    // Recharacterize the original segment as a shorter plain DataSegment.
    *it = LoadInfo::DataSegment{
        segment->offset(),
        segment->vaddr(),
        new_filesz,
        new_filesz,
    };

    if (zero_fill_size > 0) {
      // Insert a second segment for the whole pages of zero-fill.
      auto diagnostics = GetDiagnostics();
      auto inserted = load_info_.segments().insert(
          diagnostics, "ELF segments for zero-fill normalization", ++it,
          LoadInfo::ZeroFillSegment{zero_fill_vaddr, zero_fill_size});
      if (!inserted) {
        return fit::error{ElfImage::Error{"allocation failure"}};
      }
      it = *inserted;
    }
  }
  return fit::ok();
}

void ElfImage::Printf() const {
  printf("%s: In ELF file \"%.*s/%.*s\" from STORAGE_KERNEL item BOOTFS: ", gSymbolize->name(),
         static_cast<int>(package_.size()), package_.data(), static_cast<int>(name_.size()),
         name_.data());
}

void ElfImage::Printf(const char* fmt, ...) const {
  Printf();
  va_list args;
  va_start(args, fmt);
  vprintf(fmt, args);
  va_end(args);
}

void ElfImage::Printf(Error error) const {
  Printf();
  zbitl::PrintBootfsError(error);
}
