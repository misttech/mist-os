# Minimal UndefinedBehaviorSanitizer for custom embedded uses

This is a small header-only library that makes it simple to define the runtime
required by (UndefinedBehaviorSanitizer)[ubsan].  The runtime implementations
in LLVM's compiler-rt, even the "minimal" one, cannot fit into custom build
situations such as kernel or embedded code, for a variety of reasons.

**TODO(https://fxbug.dev/334165273):**
This may be upstreamed into LLVM in the future, removing the need
for this separate header library.

[ubsan]: https://clang.llvm.org/docs/UndefinedBehaviorSanitizer.html
