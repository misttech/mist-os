// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// IWYU pragma: private, include "src/storage/lib/trace/trace.h"

#ifndef SRC_STORAGE_LIB_TRACE_TRACE_DISABLED_H_
#define SRC_STORAGE_LIB_TRACE_TRACE_DISABLED_H_

#include <cstddef>
#include <cstdint>
#include <type_traits>

#include <fbl/string_traits.h>

namespace storage::trace {
namespace internal {

// TraceArg is defined for the argument types accepted by the real tracing library.
inline void TraceArg(int32_t arg) {}
inline void TraceArg(uint32_t arg) {}
inline void TraceArg(int64_t arg) {}
inline void TraceArg(uint64_t arg) {}
inline void TraceArg(double arg) {}
inline void TraceArg(bool arg) {}
template <typename T>
inline void TraceArg(const T* arg) {}
template <typename T, typename = std::enable_if_t<fbl::is_string_like_v<T>>>
inline void TraceArg(const T& arg) {}

// Base case for the recursive variadic template expansion.
inline void TraceArgs() {}

// Ensures that all of the argument names are string literals and all of the argument types will be
// accepted by the real tracing library.
template <size_t NameSize, typename Arg, typename... Args>
inline void TraceArgs(const char (&name)[NameSize], Arg arg, Args&&... args) {
  TraceArg(arg);
  TraceArgs(std::forward<Args>(args)...);
}

// Mimics the type checking done on the real duration macros to ensure that code written without
// tracing enabled will compile when tracing is enabled.
template <size_t CategorySize, size_t NameSize, typename... Args>
inline void TraceDuration(const char (&category)[CategorySize], const char (&name)[NameSize],
                          Args&&... args) {
  TraceArgs(std::forward<Args>(args)...);
}

// Mimics the type checking done on the real flow macros to ensure that code written without tracing
// enabled will compile when tracing is enabled.
template <size_t CategorySize, size_t NameSize, typename... Args>
inline void TraceFlow(const char (&category)[CategorySize], const char (&name)[NameSize],
                      uint64_t flow_id, Args&&... args) {
  TraceArgs(std::forward<Args>(args)...);
}

}  // namespace internal

// Generates a trace ID that will be unique across the system (barring overflow of the per-process
// nonce, reuse of a zx_handle_t for two processes, or some other code in this process which uses
// the same procedure to generate IDs).
//
// We use this instead of the standard TRACE_NONCE because TRACE_NONCE is only unique within a
// process; we need IDs that are unique across all processes.
inline uint64_t GenerateTraceId() { return 0; }

}  // namespace storage::trace

#define TRACE_DURATION(category, name, ...) \
  ::storage::trace::internal::TraceDuration(category, name, ##__VA_ARGS__)

#define TRACE_DURATION_BEGIN(category, name, ...) \
  ::storage::trace::internal::TraceDuration(category, name, ##__VA_ARGS__)

#define TRACE_DURATION_END(category, name, ...) \
  ::storage::trace::internal::TraceDuration(category, name, ##__VA_ARGS__)

#define TRACE_FLOW_BEGIN(category, name, flow_id, ...) \
  ::storage::trace::internal::TraceFlow(category, name, flow_id, ##__VA_ARGS__)

#define TRACE_FLOW_STEP(category, name, flow_id, ...) \
  ::storage::trace::internal::TraceFlow(category, name, flow_id, ##__VA_ARGS__)

#define TRACE_FLOW_END(category, name, flow_id, ...) \
  ::storage::trace::internal::TraceFlow(category, name, flow_id, ##__VA_ARGS__)

#define TRACE_NONCE() 0

inline uint64_t GenerateTraceId() { return 0; }

#endif  // SRC_STORAGE_LIB_TRACE_TRACE_DISABLED_H_
