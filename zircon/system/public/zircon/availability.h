// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_AVAILABILITY_H_
#define ZIRCON_AVAILABILITY_H_

// Availability attributes.

#if defined(__Fuchsia_API_level__) && defined(__clang__)

// An API that was added to the platform.
//
// Annotates the API level at which the API was added to the platform. Use
// ZX_DEPRECATED_SINCE if the API is later deprecated.
//
// Example:
//
//   void fdio_spawn(...) ZX_AVAILABLE_SINCE(4);
//
#define ZX_AVAILABLE_SINCE(level_added) \
  __attribute__((availability(fuchsia, strict, introduced = level_added)))

// An API that was added the platform and later deprecated.
//
// Annotates the API level at which the API added the platform and the API
// level at which the API was deprecated.
//
// Deprecated API can still be called by clients. The deprecation annotation
// is a warning that the API is likely to be removed in the future. APIs should
// be deprecated for at least one API level before being removed.
//
// Use the `msg` parameter to explain why the API was deprecated and what
// clients should do instead of using the API.
//
// Example:
//
//   void fdio_fork(...) ZX_DEPRECATED_SINCE(1, 4,
//       "Root cause of security vulnerabilities due to implicit handle "
//       "transfer. Use fdio_spawn instead.");
//
#define ZX_DEPRECATED_SINCE(level_added, level_deprecated, msg)          \
  __attribute__((availability(fuchsia, strict, introduced = level_added, \
                              deprecated = level_deprecated, message = msg)))

// An API that was added to the platform and later removed.
//
// Annotates the API level at which the API added the platform, the API
// level at which the API was deprecated, and the API level at which the API
// was removed.
//
// Clients can no longer call APIs if they are compiled to target an API
// level at, or beyond, the level at which the API was removed. APIs should be
// deprecated for at least one API level before being removed.
//
// Example:
//
//   void fdio_fork(...) ZX_REMOVED_SINCE(1, 4, 8,
//       "Root cause of security vulnerabilities due to implicit handle "
//       "transfer. Use fdio_spawn instead.");
//
#define ZX_REMOVED_SINCE(level_added, level_deprecated, level_removed, msg)             \
  __attribute__((availability(fuchsia, strict, introduced = level_added,                \
                              deprecated = level_deprecated, obsoleted = level_removed, \
                              message = msg)))

#else  // __Fuchsia_API_level__

#define ZX_AVAILABLE_SINCE(level_added)
#define ZX_DEPRECATED_SINCE(level_added, level_deprecated, msg)
#define ZX_REMOVED_SINCE(level_added, level_deprecated, level_removed, msg)

#endif  // __Fuchsia_API_level__

// To avoid mistakenly using a non-existent name or unpublished API level, the levels specified in
// the following macros are converted to calls to a macro containing the specified API level in its
// name. If the macro does not exist, the build will fail. See https://fxbug.dev/42084512.
#define FUCHSIA_API_LEVEL_CAT_INDIRECT_(prefix, level) prefix##level##_
#define FUCHSIA_API_LEVEL_CAT_(prefix, level) FUCHSIA_API_LEVEL_CAT_INDIRECT_(prefix, level)
#define FUCHSIA_API_LEVEL_(level) (FUCHSIA_API_LEVEL_CAT_(FUCHSIA_INTERNAL_LEVEL_, level)())

// Macros for conditionally compiling code based on the target API level.
// Prefer the attribute macros above for declarations.
//
// Use to guard code that is added and/or removed at specific API levels.
// `level` must be a one of:
//  * a positive decimal integer literal no greater than the largest current stable API level
//    * Supported levels are defined in availability_levels.h.
//    * Support for older retired API levels may be removed over time.
//  * `NEXT` - for code to be included in the next stable API level.
//    * TODO(https://fxbug.dev/323889271): Add support for NEXT as a parameter.
//  * `HEAD` - for either of the following:
//    * In-development code that is not ready to be exposed in an SDK
//  * * Code that should only be in Platform builds.

// The target API level is `level` or greater.
#define FUCHSIA_API_LEVEL_AT_LEAST(level) (__Fuchsia_API_level__ >= FUCHSIA_API_LEVEL_(level))

// The target API level is less than `level`.
#define FUCHSIA_API_LEVEL_LESS_THAN(level) (__Fuchsia_API_level__ < FUCHSIA_API_LEVEL_(level))

// The target API level is `level` or less.
#define FUCHSIA_API_LEVEL_AT_MOST(level) (__Fuchsia_API_level__ <= FUCHSIA_API_LEVEL_(level))

// Do NOT use this value directly. Use one of the macros above instead.
// The value of __Fuchsia_API_level__ when the target API level is HEAD.
#define FUCHSIA_INTERNAL_USE_ONLY_FUCHSIA_HEAD_() 4292870144

// Obsolete mechanism for determining whether the target API level is HEAD.
// Use one of the macros above instead.
// TODO(https://fxbug.dev/42084512): Remove FUCHSIA_HEAD once all code is using
// the macros above. In the short term, consider defining it as a string like
// "do not use" so uses of it will fail to compile.
#define FUCHSIA_HEAD FUCHSIA_INTERNAL_USE_ONLY_FUCHSIA_HEAD_()

// The macros referenced by the output of `FUCHSIA_API_LEVEL_()` must be defined for each API level.
// They are defined in the following file, which must be included after the macros above because it
// may use those macros.
#include <zircon/availability_levels.inc>

#endif  // ZIRCON_AVAILABILITY_H_
