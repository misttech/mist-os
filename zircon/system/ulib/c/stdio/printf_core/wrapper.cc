// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wrapper.h"

// TODO(https://fxbug.dev/42105189): These are defined as macros in
// <zircon/compiler.h> and used by some other headers such as in libzx.  This
// conflicts with their use as scoped identifiers in the llvm-libc code reached
// from this header, when this header is included in someplace that also
// includes <zircon/compiler.h> and those headers that rely on its macros.
// <zircon/compiler.h> should not be defining macros in the public namespace
// this way, but until that's fixed work around the issue by hiding the macros
// during the evaluation of the llvm-libc headers.
#pragma push_macro("add_overflow")
#undef add_overflow
#pragma push_macro("sub_overflow")
#undef sub_overflow

#include "src/stdio/printf_core/printf_main.h"

// TODO(https://fxbug.dev/42105189): See comment above.
#pragma pop_macro("add_overflow")
#pragma pop_macro("sub_overflow")

namespace LIBC_NAMESPACE::printf_core {

constexpr cpp::string_view kNewline = "\n";
constexpr cpp::string_view kEmpty = "";

#ifdef LIBC_COPT_PRINTF_RUNTIME_DISPATCH
using FlushToStreamWriteBuffer = WriteBuffer<WriteMode::FLUSH_TO_STREAM>;
#else
using FlushToStreamWriteBuffer = WriteBuffer;
#endif

int PrintfImpl(int (*write)(std::string_view str, void* hook), void* hook, std::span<char> buffer,
               PrintfNewline newline, const char* format, va_list args) {
  struct WriteBufferHook {
    int (*write)(std::string_view str, void* hook);
    void* hook;
  } write_buffer_hook = {.write = write, .hook = hook};

  FlushToStreamWriteBuffer write_buffer{
      buffer.data(),
      buffer.size(),
      [](cpp::string_view str, void* arg) -> int {
        auto [write, hook] = *static_cast<const WriteBufferHook*>(arg);
        return write({str.data(), str.size()}, hook);
      },
      &write_buffer_hook,
  };
#ifdef LIBC_COPT_PRINTF_RUNTIME_DISPATCH
  Writer<WriteMode::FLUSH_TO_STREAM> writer{write_buffer};
#else
  Writer writer{&write_buffer};
#endif

  internal::ArgList arg_list{args};
  int result = printf_main(&writer, format, arg_list);
  write_buffer.overflow_write(newline == PrintfNewline::kYes ? kNewline : kEmpty);

  return result;
}

}  // namespace LIBC_NAMESPACE::printf_core
