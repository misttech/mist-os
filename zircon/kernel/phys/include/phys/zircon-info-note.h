// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZIRCON_INFO_NOTE_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZIRCON_INFO_NOTE_H_

#include <lib/elfldltl/layout.h>
#include <lib/special-sections/special-sections.h>

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <type_traits>

// For some literal type T, write:
// ```
// ZIRCON_INFO_NOTE ZirconInfoNote<T{...}> info_note;
// ```

constexpr char kZirconInfoNoteName[] = "ZirconInfo";

template <auto Contents, uint32_t NoteType = 0>
  requires(std::is_trivially_copyable_v<decltype(Contents)> &&
           std::is_trivially_destructible_v<decltype(Contents)> &&
           alignof(decltype(Contents)) <= sizeof(uint32_t))
struct ZirconInfoNote {
  constexpr explicit ZirconInfoNote()
      : nhdr({
            .namesz = sizeof(name),
            .descsz = sizeof(contents),
            .type = NoteType,
        }),
        contents(Contents) {
    std::ranges::copy(kZirconInfoNoteName, name.begin());
  }

  elfldltl::Elf<>::Nhdr nhdr;
  std::array<char, sizeof(kZirconInfoNoteName)> name;
  alignas(uint32_t) decltype(Contents) contents;
};

#define ZIRCON_INFO_NOTE SPECIAL_SECTION(".note.ZirconInfo", uint32_t) constinit const

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZIRCON_INFO_NOTE_H_
