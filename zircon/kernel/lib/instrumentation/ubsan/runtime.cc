// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/crashlog.h>
#include <lib/ubsan-custom/handlers.h>
#include <platform.h>
#include <stdio.h>

ubsan::Report::Report(const char* check, const ubsan::SourceLocation& loc,  //
                      void* caller, void* frame) {
  platform_panic_start();
  fprintf(&stdout_panic_buffer,
          "\n"
          "*** KERNEL PANIC (caller pc: %p, stack frame: %p):\n"
          "*** ",
          caller, frame);

  fprintf(&stdout_panic_buffer, "UBSAN CHECK FAILED at (%s:%d): %s\n", loc.filename, loc.line,
          check);
}

ubsan::Report::~Report() {
  fprintf(&stdout_panic_buffer, "\n");
  platform_halt(HALT_ACTION_HALT, ZirconCrashReason::Panic);
}

void ubsan::VPrintf(const char* fmt, va_list args) { vprintf(fmt, args); }
