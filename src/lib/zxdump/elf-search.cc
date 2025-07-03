// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/zxdump/elf-search.h"

#include "core.h"

namespace zxdump {

bool IsLikelyElfMapping(const zx_info_maps_t& maps) {
  const bool readonly =
      (maps.u.mapping.mmu_flags & (ZX_VM_PERM_READ | ZX_VM_PERM_WRITE)) == ZX_VM_PERM_READ;

  // It might be ELF if it...
  return maps.type == ZX_INFO_MAPS_TYPE_MAPPING &&
         maps.u.mapping.vmo_offset == 0 &&  // is the start of the file (VMO),
         readonly &&                        // maps it read-only,
         maps.size > sizeof(Elf::Ehdr);     // ... and has space for ELF,
}

// Try to detect an ELF image in this segment.  If one is found and its phdrs
// are accessible, return a view into them from process.read_memory.
fit::result<Error, zxdump::Buffer<Elf::Phdr>> DetectElf(Process& process,
                                                        const zx_info_maps_t& segment) {
  constexpr auto no_elf = []() { return fit::ok(zxdump::Buffer<Elf::Phdr>()); };

  if (segment.type != ZX_INFO_MAPS_TYPE_MAPPING) {
    return no_elf();
  }

  auto result = process.read_memory<Elf::Ehdr>(segment.base);
  if (result.is_error()) {
    return result.take_error();
  }

  const Elf::Ehdr& ehdr = result.value();
  if (ehdr.Valid() && ehdr.phentsize == sizeof(Elf::Phdr) && ehdr.phnum > 0 &&
      ehdr.phnum != Elf::Ehdr::kPnXnum && (ehdr.phoff % alignof(Elf::Phdr)) == 0 &&
      ehdr.phoff < segment.size && segment.size - ehdr.phoff >= ehdr.phnum * sizeof(Elf::Phdr)) {
    auto phdr_result = process.read_memory<Elf::Phdr>(segment.base + ehdr.phoff, ehdr.phnum);
    if (phdr_result.is_error()) {
      return phdr_result.take_error();
    }
    return fit::ok(*std::move(phdr_result));
  }

  return no_elf();
}

fit::result<Error, ElfIdentity> DetectElfIdentity(Process& process, const zx_info_maps_t& segment,
                                                  std::span<const Elf::Phdr> phdrs) {
  // Find the first PT_LOAD segment.
  std::optional<uint64_t> first_load;
  for (const auto& phdr : phdrs) {
    if (phdr.type == elfldltl::ElfPhdrType::kLoad) {
      first_load = phdr.vaddr & -process.dump_page_size();
      break;
    }
  }
  if (!first_load) {
    // This is not really a valid ELF image, probably.
    return fit::ok(ElfIdentity{});
  }

  // Note the notes.
  std::vector<SegmentDisposition::Note> notes;
  std::optional<SegmentDisposition::Note> dynamic;
  for (const auto& phdr : phdrs) {
    if (phdr.type == elfldltl::ElfPhdrType::kNote && phdr.vaddr >= *first_load) {
      // It's an allocated note segment that might be within this segment.
      const size_t offset = phdr.vaddr - *first_load;
      if (offset < segment.size && segment.size - offset >= phdr.filesz) {
        // It actually fits inside the segment, so we can examine it.
        notes.push_back({phdr.vaddr, phdr.filesz});
      }
    } else if (phdr.type == elfldltl::ElfPhdrType::kDynamic && phdr.vaddr >= *first_load) {
      dynamic = {phdr.vaddr, phdr.filesz};
    }
  }
  // The phdrs view is no longer used now, so it's safe to use read_memory.

  ElfIdentity id;

  // Detect the build ID.
  constexpr std::string_view kBuildIdNoteName{
      "GNU", sizeof("GNU"),  // NUL terminator included!
  };

  for (auto note : notes) {
    note.vaddr += segment.base - *first_load;
    while (note.size >= sizeof(Elf::Nhdr)) {
      Elf::Nhdr nhdr;
      if (auto result = process.read_memory<Elf::Nhdr>(note.vaddr); result.is_error()) {
        return result.take_error();
      } else {
        nhdr = result.value();
      }

      const size_t this_note_size = sizeof(nhdr) + NoteAlign(nhdr.namesz) + NoteAlign(nhdr.descsz);
      if (this_note_size > note.size) {
        break;
      }

      if (nhdr.type == static_cast<uint32_t>(elfldltl::ElfNoteType::kGnuBuildId) &&
          nhdr.descsz > 0 && nhdr.namesz() == kBuildIdNoteName.size()) {
        auto result = process.read_memory<char, std::string_view>(note.vaddr + sizeof(Elf::Nhdr),
                                                                  kBuildIdNoteName.size());
        if (result.is_error()) {
          return result.take_error();
        }

        ZX_DEBUG_ASSERT(result.value()->size() == kBuildIdNoteName.size());
        if (**result == kBuildIdNoteName) {
          // We have a winner!
          note.size = this_note_size;
          id.build_id = note;
          break;
        }
      }

      note.vaddr += this_note_size;
      note.size -= this_note_size;
    }
  }

  // Detect the SONAME.  Ignore any memory errors since just returning
  // a build ID and no SONAME is better than returning no information at all.
  if (dynamic && dynamic->size > sizeof(Elf::Dyn)) {
    dynamic->vaddr += segment.base - *first_load;
    dynamic->size /= sizeof(Elf::Dyn);
    auto read = process.read_memory<Elf::Dyn>(dynamic->vaddr, dynamic->size);
    if (read.is_ok()) {
      std::optional<uint64_t> soname, strtab;
      for (const auto& dyn : **read) {
        switch (dyn.tag) {
          case elfldltl::ElfDynTag::kSoname:
            soname = dyn.val;
            break;
          case elfldltl::ElfDynTag::kStrTab:
            strtab = segment.base - *first_load + dyn.val;
            break;
          default:
            break;
        }
        if (soname && strtab) {
          uint64_t vaddr = *strtab + *soname;
          auto string = process.read_memory_string(vaddr);
          if (string.is_ok()) {
            id.soname = {vaddr, string.value().size()};
          }
          break;
        }
      }
    }
  }

  return fit::ok(id);
}

std::pair<MapsInfoSpanIterator, MapsInfoSpanIterator> ElfSearch(  //
    Process& process, MapsInfoSpanIterator first, MapsInfoSpanIterator last) {
  while (first < last) {
    const auto& segment = *first;
    if (IsLikelyElfMapping(segment)) {
      auto result = DetectElf(process, segment);
      if (result.is_error()) {
        return {first, first};
      }
      if (!result->empty()) {
        std::optional<uint64_t> vaddr_base, vaddr_limit;
        auto phdrs = *result.value();
        const uint64_t page_size = process.dump_page_size();
        for (const auto& phdr : phdrs) {
          if (phdr.type == elfldltl::ElfPhdrType::kLoad) {
            if (!vaddr_base) {
              vaddr_base = phdr.vaddr & -page_size;
            }
            if (phdr.vaddr < *vaddr_base) {
              // It's invalid to have non-ascending segments.
              // Consider this not to be a real ELF image at all.
              vaddr_limit.reset();
              break;
            }
            const uint64_t limit =
                (phdr.vaddr - *vaddr_base + segment.base + phdr.memsz + page_size - 1) & -page_size;
            if (limit < segment.base) {
              // Overflow is invalid.
              vaddr_limit.reset();
              break;
            }
            if (vaddr_limit && limit < *vaddr_limit) {
              // It's invalid to have overlapping segments.
              vaddr_limit.reset();
              break;
            }
            vaddr_limit = limit;
          }
        }
        if (vaddr_limit) {
          // We have an image.
          auto last_segment = first;
          do {
            ++last_segment;
          } while (last_segment != last && last_segment->base < *vaddr_limit);
          return {first, last_segment};
        }
      }
    }
    ++first;
  }
  return {last, last};
}

}  // namespace zxdump
