// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

//===-- asan_poisoning.cpp ------------------------------------------------===//
//
// Part of the LLVM Project, under the Apache License v2.0 with LLVM Exceptions.
// See https://llvm.org/LICENSE.txt for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// This file is a part of AddressSanitizer, an address sanity checker.
//
// Shadow memory poisoning by ASan RTL and by user application.
//===----------------------------------------------------------------------===//

#include <align.h>
#include <platform.h>
#include <zircon/assert.h>

#include <ktl/algorithm.h>
#include <sanitizer/asan_interface.h>

#include "asan-internal.h"

#include <ktl/enforce.h>

static inline bool AddrIsAlignedByGranularity(uintptr_t a) {
  return (a & (kAsanGranularity - 1)) == 0;
}

static void FixUnalignedStorage(uintptr_t storage_beg, uintptr_t storage_end, uintptr_t& old_beg,
                                uintptr_t& old_end, uintptr_t& new_beg, uintptr_t& new_end) {
  constexpr uintptr_t granularity = kAsanGranularity;
  if (unlikely(!AddrIsAlignedByGranularity(storage_end))) {
    uintptr_t end_down = ROUNDDOWN(storage_end, granularity);
    // Ignore the last unaligned granule if the storage is followed by
    // unpoisoned byte, because we can't poison the prefix anyway. Don't call
    // AddressIsPoisoned at all if container changes does not affect the last
    // granule at all.
    if ((((old_end != new_end) && ktl::max(old_end, new_end) > end_down) ||
         ((old_beg != new_beg) && ktl::max(old_beg, new_beg) > end_down)) &&
        !asan_address_is_poisoned(storage_end)) {
      old_beg = ktl::min(end_down, old_beg);
      old_end = ktl::min(end_down, old_end);
      new_beg = ktl::min(end_down, new_beg);
      new_end = ktl::min(end_down, new_end);
    }
  }

  // Handle misaligned begin and cut it off.
  if (unlikely(!AddrIsAlignedByGranularity(storage_beg))) {
    uintptr_t beg_up = ROUNDUP(storage_beg, granularity);
    // The first unaligned granule needs special handling only if we had bytes
    // there before and will have none after.
    if ((new_beg == new_end || new_beg >= beg_up) && old_beg != old_end && old_beg < beg_up) {
      // Keep granule prefix outside of the storage unpoisoned.
      uintptr_t beg_down = ROUNDDOWN(storage_beg, granularity);
      *addr2shadow(beg_down) = static_cast<uint8_t>(storage_beg - beg_down);
      old_beg = ktl::max(beg_up, old_beg);
      old_end = ktl::max(beg_up, old_end);
      new_beg = ktl::max(beg_up, new_beg);
      new_end = ktl::max(beg_up, new_end);
    }
  }
}

// This is neded by std::vector / std::string  when asan is enabled.
void __sanitizer_annotate_contiguous_container(const void* beg_p, const void* end_p,
                                               const void* old_mid_p, const void* new_mid_p) {
  uintptr_t storage_beg = reinterpret_cast<uintptr_t>(beg_p);
  uintptr_t storage_end = reinterpret_cast<uintptr_t>(end_p);
  uintptr_t old_end = reinterpret_cast<uintptr_t>(old_mid_p);
  uintptr_t new_end = reinterpret_cast<uintptr_t>(new_mid_p);
  uintptr_t old_beg = storage_beg;
  uintptr_t new_beg = storage_beg;
  uintptr_t granularity = kAsanGranularity;

  if (!(storage_beg <= old_end && storage_beg <= new_end && old_end <= storage_end &&
        new_end <= storage_end)) {
    printf(
        "KASAN bad parameters to\n"
        "__sanitizer_annotate_contiguous_container:\n"
        "      beg     : %p\n"
        "      end     : %p\n"
        "      old_mid : %p\n"
        "      new_mid : %p\n",
        (void*)storage_beg, (void*)storage_end, (void*)old_end, (void*)new_end);
    if (!IS_ALIGNED(storage_beg, granularity))
      printf("ERROR: beg is not aligned by %zu\n", granularity);
    panic("kasan\n");
  }

  /*
  CHECK_LE(storage_end - storage_beg,
           FIRST_32_SECOND_64(1UL << 30, 1ULL << 40));  // Sanity check.*/

  if (old_end == new_end)
    return;  // Nothing to do here.

  FixUnalignedStorage(storage_beg, storage_end, old_beg, old_end, new_beg, new_end);

  uintptr_t a = ROUNDDOWN(ktl::min(old_end, new_end), granularity);
  uintptr_t c = ROUNDUP(ktl::max(old_end, new_end), granularity);
  uintptr_t d1 = ROUNDDOWN(old_end, granularity);
  // uintptr_t d2 = ROUNDUP(old_mid, granularity);
  // Currently we should be in this state:
  // [a, d1) is good, [d2, c) is bad, [d1, d2) is partially good.
  // Make a quick sanity check that we are indeed in this state.
  //
  // FIXME: Two of these three checks are disabled until we fix
  // https://github.com/google/sanitizers/issues/258.
  // if (d1 != d2)
  //  DCHECK_EQ(*(u8*)MemToShadow(d1), old_mid - d1);
  //
  if (a + granularity <= d1)
    DEBUG_ASSERT(*(uint8_t*)addr2shadow(a) == 0);

  // if (d2 + granularity <= c && c <= end)
  //   DCHECK_EQ(*(u8 *)MemToShadow(c - granularity),
  //            kAsanContiguousContainerOOBMagic);

  uintptr_t b1 = ROUNDDOWN(new_end, granularity);
  uintptr_t b2 = ROUNDUP(new_end, granularity);
  // New state:
  // [a, b1) is good, [b2, c) is bad, [b1, b2) is partially good.
  if (b1 > a)
    asan_unpoison_shadow(a, b1 - a);
  else if (c > b2)
    asan_poison_shadow(b2, c - b2, kAsanContiguousContainerOOBMagic);
  if (b1 != b2) {
    ASSERT(b2 - b1 == granularity);
    *(uint8_t*)addr2shadow(b1) = static_cast<uint8_t>(new_end - b1);
  }
}

void __sanitizer_annotate_double_ended_contiguous_container(const void* storage_beg_p,
                                                            const void* storage_end_p,
                                                            const void* old_container_beg_p,
                                                            const void* old_container_end_p,
                                                            const void* new_container_beg_p,
                                                            const void* new_container_end_p) {
  uintptr_t storage_beg = reinterpret_cast<uintptr_t>(storage_beg_p);
  uintptr_t storage_end = reinterpret_cast<uintptr_t>(storage_end_p);
  uintptr_t old_beg = reinterpret_cast<uintptr_t>(old_container_beg_p);
  uintptr_t old_end = reinterpret_cast<uintptr_t>(old_container_end_p);
  uintptr_t new_beg = reinterpret_cast<uintptr_t>(new_container_beg_p);
  uintptr_t new_end = reinterpret_cast<uintptr_t>(new_container_end_p);

  constexpr uintptr_t granularity = kAsanGranularity;

  if (!(old_beg <= old_end && new_beg <= new_end) ||
      !(storage_beg <= new_beg && new_end <= storage_end) ||
      !(storage_beg <= old_beg && old_end <= storage_end)) {
    printf(
        "KASAN bad parameters to\n"
        "__sanitizer_annotate_double_ended_contiguous_container:\n"
        "      storage: [%p, %p)\n"
        "      old_container: [%p, %p)\n"
        "      new_container: [%p, %p)\n",
        (void*)storage_beg, (void*)storage_end, (void*)old_beg, (void*)old_end, (void*)new_beg,
        (void*)new_end);
    if (!IS_ALIGNED(storage_beg, granularity))
      printf("ERROR: beg is not aligned by %zu\n", granularity);
    panic("kasan\n");
  }

  if ((old_beg == old_end && new_beg == new_end) || (old_beg == new_beg && old_end == new_end))
    return;  // Nothing to do here.

  FixUnalignedStorage(storage_beg, storage_end, old_beg, old_end, new_beg, new_end);

  // Handle non-intersecting new/old containers separately
  if (old_beg == old_end || new_beg == new_end || new_end <= old_beg || old_end <= new_beg) {
    if (old_beg != old_end) {
      // Poison the old container
      uintptr_t a = ROUNDDOWN(old_beg, granularity);
      uintptr_t b = ROUNDUP(old_end, granularity);
      asan_poison_shadow(a, b - a, kAsanContiguousContainerOOBMagic);
    }

    if (new_beg != new_end) {
      // Unpoison the new container
      uintptr_t a = ROUNDDOWN(new_beg, granularity);
      uintptr_t b = ROUNDDOWN(new_end, granularity);
      asan_unpoison_shadow(a, b - a);
      if (!IS_ALIGNED(new_end, granularity))
        *(uint8_t*)addr2shadow(b) = static_cast<uint8_t>(new_end - b);
    }
    return;
  }

  // Handle intersecting containers
  if (new_beg < old_beg) {
    uintptr_t a = ROUNDDOWN(new_beg, granularity);
    uintptr_t c = ROUNDDOWN(old_beg, granularity);
    asan_unpoison_shadow(a, c - a);
  } else if (new_beg > old_beg) {
    uintptr_t a = ROUNDDOWN(old_beg, granularity);
    uintptr_t c = ROUNDDOWN(new_beg, granularity);
    asan_poison_shadow(a, c - a, kAsanContiguousContainerOOBMagic);
  }

  if (new_end > old_end) {
    uintptr_t a = ROUNDDOWN(old_end, granularity);
    uintptr_t c = ROUNDDOWN(new_end, granularity);
    asan_unpoison_shadow(a, c - a);
    if (!IS_ALIGNED(new_end, granularity))
      *(uint8_t*)addr2shadow(c) = static_cast<uint8_t>(new_end - c);
  } else if (new_end < old_end) {
    uintptr_t a2 = ROUNDUP(new_end, granularity);
    uintptr_t c2 = ROUNDUP(old_end, granularity);
    asan_poison_shadow(a2, c2 - a2, kAsanContiguousContainerOOBMagic);

    if (!IS_ALIGNED(new_end, granularity)) {
      uintptr_t a = ROUNDDOWN(new_end, granularity);
      *(uint8_t*)addr2shadow(a) = static_cast<uint8_t>(new_end - a);
    }
  }
}
