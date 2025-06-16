// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ZXDUMP_INCLUDE_LIB_ZXDUMP_ELF_SEARCH_H_
#define SRC_LIB_ZXDUMP_INCLUDE_LIB_ZXDUMP_ELF_SEARCH_H_

#include <lib/elfldltl/layout.h>
#include <lib/fit/result.h>
#include <zircon/syscalls/object.h>

#include <optional>
#include <span>
#include <string>
#include <utility>

#include "buffer.h"
#include "dump.h"
#include "task.h"
#include "types.h"

namespace zxdump {

// Zircon core dumps are always in the 64-bit little-endian ELF format.
using Elf = elfldltl::Elf64<elfldltl::ElfData::k2Lsb>;

// This can be used from a zxdump::SegmentCallback function to guess if it's
// likely worthwhile to probe this mapping for containing the beginning of an
// ELF image.  If this returns true, a zxdump::ProcessDump::FindBuildIdNote
// call on the segment is recommended.
bool IsLikelyElfMapping(const zx_info_maps_t& maps);

// These helpers are used by zxdump::ProcessDump::FindBuildIdNote, which is
// used in `prune_segment` callbacks for zxdump::ProcessDump::CollectProcess.
// But they can also be used separately.

// Try to detect an ELF image in this segment.  If one is found and its phdrs
// are accessible, return a view into them from process.read_memory (which see
// about the lifetime of the buffer).  Its ELF header can be fetched with
// process.read_memory<Elf::Ehdr>(segment.base).
fit::result<Error, Buffer<Elf::Phdr>> DetectElf(  //
    Process& process, const zx_info_maps_t& segment);

// This is the return value of zxdump::DetectElfIdentity, below.  It contains
// absolute memory ranges in the process that provide identifying information.
struct ElfIdentity {
  static constexpr size_t kBuildIdOffset = sizeof(Elf::Nhdr) + sizeof("GNU");

  // This gives the vaddr and size of the ELF build ID note in memory.  The
  // size is zero if no build ID note was found.  The memory there can be read
  // out and the note header parsed to find the ID payload.  The note header
  // and name together have a fixed known size before the build ID bytes so
  // that can just be skipped to extract only the actual ID bytes.
  SegmentDisposition::Note build_id;

  // This gives the vaddr and size of the DT_SONAME string (without its NUL
  // terminator).  The size is zero if no valid DT_SONAME string was found.
  // (This reuses the vaddr, size pair type for the string's range in memory,
  // but note not a note.)
  SegmentDisposition::Note soname;
};

// Given a detected ELF image, examine its program headers and memory to find
// the build ID note and DT_SONAME string if possible.  The argument span
// doesn't have to be one returned by DetectElf (i.e. by process.read_memory)
// but it can be.
fit::result<Error, ElfIdentity> DetectElfIdentity(  //
    Process& process, const zx_info_maps_t& segment, std::span<const Elf::Phdr> phdrs);

// This uses zxdump::DetectElf to search a range of the address space for an
// ELF image.  It returns a {first, last} pair giving the [first, last)
// subrange of the argument range that holds an ELF image.  It returns the pair
// {last, last} if none is found.  If it encounters errors, it returns {it, it}
// pointing to the segment where zxdump::DetectElf or zxdump::DetectElfIdentity
// returned an error.
using MapsInfoSpanIterator = std::span<const zx_info_maps_t>::iterator;
using ElfSearchResult = std::pair<MapsInfoSpanIterator, MapsInfoSpanIterator>;
ElfSearchResult ElfSearch(Process& process, MapsInfoSpanIterator first, MapsInfoSpanIterator last);

}  // namespace zxdump

#endif  // SRC_LIB_ZXDUMP_INCLUDE_LIB_ZXDUMP_ELF_SEARCH_H_
