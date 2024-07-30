// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "log.h"

#include <lib/boot-options/boot-options.h>
#include <zircon/limits.h>

Log* gLog;

int Log::Write(ktl::string_view str) {
  // Always write to the live console first.
  if (mirror_) {
    mirror_.Write(str);
  }

  AppendToLog(str);

  return static_cast<int>(str.size());
}

void Log::SetStdout() {
  ZX_ASSERT(*stdout != FILE{this});
  ZX_ASSERT(!mirror_);
  mirror_ = *stdout;
  *stdout = FILE{this};
}

void Log::RestoreStdout() {
  if (*stdout == FILE{this}) {
    *stdout = mirror_;
    mirror_ = FILE{};
  }
}

void Log::AppendToLog(ktl::string_view str) {
  // Use the remaining space in the existing buffer, or make more space.
  auto make_space = [this](size_t needed) -> ktl::span<char> {
    if (ktl::span space = buffer_chars_left(); space.size() >= needed) {
      return space;
    }

    // Expand (or initially allocate) the buffer if it's too small.
    // The buffer is always allocated in whole pages.
    constexpr size_t kPageSize = ZX_PAGE_SIZE;
    const size_t expand_size = (needed + kPageSize - 1) & -kPageSize;
    fbl::AllocChecker ac;

    // TODO(https://fxbug.dev/347766366): While trampoline booting is in effect
    // in the x86 codepath, care needs to be taken with `Allocation`s made
    // before the fixed-address image recharacterization that TrampolineBoot
    // does. In particular, we do not want to absent-mindedly free a region
    // (e.g., due to an Allocation::Resize()) that was recharacterized as being
    // a part of a fixed-address image. Until TrampolineBoot is removed from
    // kernel boot, defensively leak the prior log buffer allocation when trying
    // to resize and instead just allocate a new one. This only happens when
    // the buffer reaches a page boundary and amounts to an infrequently
    // exercised wart in the meantime. Once we can, go back to using
    // Allocation::Resize().
    Allocation new_buffer = Allocation::New(ac, memalloc::Type::kPhysLog,
                                            buffer_.size_bytes() + expand_size, kPageSize);
    if (!ac.check()) {
      RestoreStdout();
      ZX_PANIC("failed to increase phys log from %#zx to %#zx bytes", buffer_.size_bytes(),
               buffer_.size_bytes() + expand_size);
    }
    if (buffer_) {
      memcpy(new_buffer->data(), buffer_->data(), size_);
      static_cast<void>(buffer_.release());
    }
    buffer_ = ktl::move(new_buffer);
    return buffer_chars_left();
  };

  ktl::span<char> space = make_space(str.size());
  size_t copied = str.copy(space.data(), space.size());
  ZX_DEBUG_ASSERT(copied == str.size());
  size_ += copied;
}

FILE Log::LogOnlyFile() {
  return FILE{[](void* log, ktl::string_view str) {
                static_cast<Log*>(log)->AppendToLog(str);
                return static_cast<int>(str.size());
              },
              this};
}

FILE Log::VerboseOnlyFile() {
  if (gBootOptions && !gBootOptions->phys_verbose) {
    return LogOnlyFile();
  }
  return FILE{this};
}
