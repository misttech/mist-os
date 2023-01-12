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
  // Use the remaining space in the existing buffer.
  constexpr auto as_chars = [](ktl::span<ktl::byte> bytes) -> ktl::span<char> {
    return {reinterpret_cast<char*>(bytes.data()), bytes.size()};
  };
  ktl::span<char> space = as_chars(buffer_->subspan(size_));

  // Expand (or initially allocate) the buffer if it's too small.
  if (space.size() < str.size()) {
    // The buffer is always allocated in whole pages.
    constexpr size_t kPageSize = ZX_PAGE_SIZE;
    fbl::AllocChecker ac;
    if (buffer_) {
      buffer_.Resize(ac, buffer_.size_bytes() + kPageSize);
    } else {
      buffer_ = Allocation::New(ac, memalloc::Type::kPhysLog, kPageSize, kPageSize);
    }
    if (!ac.check()) {
      RestoreStdout();
      ZX_PANIC("failed to increase phys log from %#zx to %#zx bytes", buffer_.size_bytes(),
               buffer_.size_bytes() + kPageSize);
    }
  }

  size_ += str.copy(space.data(), space.size());
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
