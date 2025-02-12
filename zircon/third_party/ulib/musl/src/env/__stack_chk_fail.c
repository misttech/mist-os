#include <lib/zircon-internal/unique-backtrace.h>

#include "libc.h"
#include "threads_impl.h"

__attribute__((__visibility__("hidden"))) uintptr_t __stack_chk_guard;

// This is called by compiler-generated function epilogue code when the stack
// frame has been clobbered by a buffer overrun or similar rogue pointer bug.
void __stack_chk_fail(void) { CRASH_WITH_UNIQUE_BACKTRACE(); }
