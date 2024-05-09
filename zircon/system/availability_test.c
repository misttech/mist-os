// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <zircon/availability.h>

// This file tests the macros defined in <zircon/availability.h> at
// __Fuchsia_API_level__. To "run the test," compile it at a variety of API
// levels, including numbered API levels and named API levels such as PLATFORM.

#if defined(BUILT_AT_NUMBERED_API_LEVEL)

// When targeting a numbered API level, we can test the macros at and around it.

// TODO(ddorwin): Remove this once API level 15, the first level in availability_levels.inc, is
// sunset and this file is thus not being built at that level.
#define FUCHSIA_INTERNAL_LEVEL_14_() 14

// Verify the preprocessor values defined by the build file.
// Since the macros only accept literals, all relative levels must be provided as literals via
// additional preprocessor values defined by the build.
#if !defined(__Fuchsia_API_level__) || __Fuchsia_API_level__ != BUILT_AT_NUMBERED_API_LEVEL
#error BUILT_AT_NUMBERED_API_LEVEL does not match __Fuchsia_API_level__.
#endif
#if BUILT_AT_NUMBERED_API_LEVEL_MINUS_ONE != (BUILT_AT_NUMBERED_API_LEVEL - 1)
#error BUILT_AT_NUMBERED_API_LEVEL_MINUS_ONE is not correctly defined.
#endif
#if BUILT_AT_NUMBERED_API_LEVEL_PLUS_ONE != (BUILT_AT_NUMBERED_API_LEVEL + 1)
#error BUILT_AT_NUMBERED_API_LEVEL_PLUS_ONE is not correctly defined.
#endif

#if !FUCHSIA_API_LEVEL_AT_LEAST(BUILT_AT_NUMBERED_API_LEVEL)
#error API level should be at least `BUILT_AT_NUMBERED_API_LEVEL`.
#endif

#if FUCHSIA_API_LEVEL_AT_LEAST(BUILT_AT_NUMBERED_API_LEVEL_PLUS_ONE)
#error API level should be less than `BUILT_AT_NUMBERED_API_LEVEL + 1`.
#endif

#if FUCHSIA_API_LEVEL_LESS_THAN(BUILT_AT_NUMBERED_API_LEVEL)
#error API level should not be less than `BUILT_AT_NUMBERED_API_LEVEL`.
#endif

#if !FUCHSIA_API_LEVEL_LESS_THAN(BUILT_AT_NUMBERED_API_LEVEL_PLUS_ONE)
#error API level should be less than `BUILT_AT_NUMBERED_API_LEVEL + 1`.
#endif

#if FUCHSIA_API_LEVEL_AT_MOST(BUILT_AT_NUMBERED_API_LEVEL_MINUS_ONE)
#error API level should greater than `BUILT_AT_NUMBERED_API_LEVEL - 1`.
#endif

#if !FUCHSIA_API_LEVEL_AT_MOST(BUILT_AT_NUMBERED_API_LEVEL)
#error API level should be no greater than `BUILT_AT_NUMBERED_API_LEVEL`.
#endif

#endif  // defined(BUILT_AT_NUMBERED_API_LEVEL)

// Always test the extreme cases. Some test cases have different results depending on whether the
// target API level is a stable numbered API level.

// Macros to assert the appropriate result depending on the current target API level.
// IFF is short for "if and only if.""
#if defined(BUILT_AT_NUMBERED_API_LEVEL)
#define EXPECT_IFF_NUMBERED_API_LEVEL(condition) \
  static_assert(condition, "`" #condition "` should be true for build at numbered API level.");
#define EXPECT_IFF_PLATFORM_BUILD(condition) \
  static_assert(!condition, "`" #condition "` should be false for build at numbered API level.");
#else
#define EXPECT_IFF_NUMBERED_API_LEVEL(condition) \
  static_assert(!condition, "`" #condition "` should be false for PLATFORM build.");
#define EXPECT_IFF_PLATFORM_BUILD(condition) \
  static_assert(condition, "`" #condition "` should be true for PLATFORM build.");
#endif  // defined(BUILT_AT_NUMBERED_API_LEVEL)

// 0x80000000
#define FIRST_RESERVED_API_LEVEL() 2147483648
// 0xFFFFFFFF + 1
#define UINT32_MAX_PLUS_ONE() 4294967296

// The tests use levels that are not supported in production. Define the necessary macros.
#define FUCHSIA_INTERNAL_LEVEL_9_() 9
#define FUCHSIA_INTERNAL_LEVEL_10000_() 10000
#define FUCHSIA_INTERNAL_LEVEL_2147483648_() 2147483648
#define FUCHSIA_INTERNAL_LEVEL_4294967296_() 4294967296

#if !(FUCHSIA_API_LEVEL_AT_LEAST(9))
#error API level should be greater than 9.
#endif

// TODO(https://fxbug.dev/323889271): Change when adding support for NEXT and HEAD to the IDK.
EXPECT_IFF_PLATFORM_BUILD(FUCHSIA_API_LEVEL_AT_LEAST(FIRST_RESERVED_API_LEVEL()))

// TODO(https://fxbug.dev/323889271): Change when adding support for NEXT and HEAD to the IDK.
EXPECT_IFF_PLATFORM_BUILD(FUCHSIA_API_LEVEL_AT_LEAST(HEAD))
EXPECT_IFF_PLATFORM_BUILD(FUCHSIA_API_LEVEL_AT_LEAST(PLATFORM))

#if FUCHSIA_API_LEVEL_AT_LEAST(UINT32_MAX_PLUS_ONE())
#error API levels are 32 bits.
#endif

#if FUCHSIA_API_LEVEL_LESS_THAN(9)
#error API level should not be less than 9.
#endif

EXPECT_IFF_NUMBERED_API_LEVEL(FUCHSIA_API_LEVEL_LESS_THAN(10000))

EXPECT_IFF_NUMBERED_API_LEVEL(FUCHSIA_API_LEVEL_LESS_THAN(HEAD))
EXPECT_IFF_NUMBERED_API_LEVEL(FUCHSIA_API_LEVEL_LESS_THAN(PLATFORM))

#if !FUCHSIA_API_LEVEL_LESS_THAN(UINT32_MAX_PLUS_ONE())
#error API levels are 32 bits.
#endif

#if FUCHSIA_API_LEVEL_AT_MOST(9)
#error API level should not be 9 or less.
#endif

EXPECT_IFF_NUMBERED_API_LEVEL(FUCHSIA_API_LEVEL_AT_MOST(10000))

#if !FUCHSIA_API_LEVEL_AT_MOST(PLATFORM)
#error API level cannot be greater than PLATFORM.
#endif

#if !FUCHSIA_API_LEVEL_AT_MOST(UINT32_MAX_PLUS_ONE())
#error API levels are 32 bits.
#endif
