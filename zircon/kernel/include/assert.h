// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2008 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_ASSERT_H_
#define ZIRCON_KERNEL_INCLUDE_ASSERT_H_

// For a description of which asserts are enabled at which debug levels, see the documentation for
// GN build argument |assert_level|.

#include <debug.h>
#include <zircon/compiler.h>

#define PANIC(args...) panic(args)

// Conditionally implement DEBUG_ASSERT based on LK_DEBUGLEVEL in kernel space.
#ifdef LK_DEBUGLEVEL

// DEBUG_ASSERT_IMPLEMENTED is intended to be used to conditionalize code that is logically part of
// a debug assert. It's useful for performing complex consistency checks that are difficult to work
// into a DEBUG_ASSERT statement.
#define DEBUG_ASSERT_IMPLEMENTED (LK_DEBUGLEVEL > 1)
#else
#define DEBUG_ASSERT_IMPLEMENTED 0
#endif

// Notes about the C++ and C versions of the assert macros.
//
// The C++ versions of these macros follow a very specific form for testing the
// assert predicate `x`.  Specifically, the test of the predicate is _always_
// written simply as `if (x)`.  Not:
//
// 1) `if (!x)`                                     // or
// 2) `if ((x))`                                    // or
// 3) `if (unlikely(!(x)))                          // or
// 4) `if (DEBUG_ASSERT_ENABLED && unlikely(!(x)))  // or, anything else.
//
// It is *always* just `if (x)`.  There is a method to this madness.  The
// primary reason for this is to catch mistakes of the following form:
//
// DEBUG_ASSERT(my_bool_variable = false);
// ASSERT(my_int_variable = 5);
//
// Both of these are examples of a pattern where a code author mean to use
// comparison (`==`), but accidentally used assignment (`=`), meaning that the
// first of these examples is always going to fire, while the second will never
// fire.  Both of them will have a side effect of mutating their variable, but a
// DEBUG_ASSERT's mutation will drop out of a release build.
//
// :: Accidental Assignment Protection ::
//
// This is all very bad, and _should_ be caught by the -Wparentheses warning
// enabled in all kernel builds.  Unfortunately, surrounding an assignment
// operation in a set of `()` will suppress the warning.  Pretty much any other
// form of the predicate test ends up requiring that we add in an extra pair of
// `()`.  This also includes wrapping the predicate in things like `unlikely(x)`
// and `likely(x)`.  So, we have to restrict ourselves strictly to testing with
// `if (x)` in order to get the accidental assignment protection we desire.
//
// :: Temporary Variable Latching ::
//
// A surprise advantage to taking this approach is that it allows us (in C++) to
// more easily write predicates which involve latching to a temporary variable.
// Consider a case where we want to debug assert something about a value
// returned by a function which is expensive to call.  We could write:
//
// DEBUG_ASSERT_MSG(Expensive() == 5,
//                  "Expensive() returned a non-five value (%u)",
//                  Expensive());
//
// But now we are calling the expensive function twice.  We could also say:
//
// [[maybe_unused]] const uint32_t e = Expensive();
// DEBUG_ASSERT_MSG(e, "Expensive() returned a non-five value (%u)", e);
//
// but it takes a couple of lines, we have to add in a [[maybe_unused]]
// annotation, and we may still be forced to evaluate Expensive if the compiler
// cannot tell that it is guaranteed to have no side effects.
//
// With the new only-`if (x)` form of testing the predicate, however, we can do
// better.  Now, we can write:
//
// DEBUG_ASSERT_MSG(uint32_t e = Expensive(); e == 5,
//                  "Expensive() returned a non-five value (%u)", e);
//
// We still get our protection against accidental assignment, we are guaranteed
// to evaluate Expensive exactly once in a debug build, and zero times in a
// release build.
//
// :: WARNING - C code does not get these benefits ::
//
// To write in this style, but also preserve the likely/unlikely hinting
// benefits, we are forced to use C++'s standardized attributes `[[likely]]` and
// `[[unlikely]]``.  This are not available in C, which has to use the old
// compiler attribute macros, which always end up introducing `()`, and
// suppressing the accidental assignment protection.  Right now, there is no
// good way around this, and as long as we are building C code using this
// header, we will need to maintain a version of these macros which do not offer
// the same level of protection.
//
#ifdef __cplusplus

// Assert that |x| is true, else panic.
//
// ASSERT is always enabled and |x| will be evaluated regardless of any build arguments.
#define ASSERT(x)                          \
  do {                                     \
    if (x) [[likely]] {                    \
    } else [[unlikely]] {                  \
      assert_fail(__FILE__, __LINE__, #x); \
    }                                      \
  } while (0)

// Assert that |x| is true, else panic with the given message.
//
// ASSERT_MSG is always enabled and |x| will be evaluated regardless of any build arguments.
#define ASSERT_MSG(x, msg, msgargs...)                         \
  do {                                                         \
    if (x) [[likely]] {                                        \
    } else [[unlikely]] {                                      \
      assert_fail_msg(__FILE__, __LINE__, #x, msg, ##msgargs); \
    }                                                          \
  } while (0)

// Assert that |x| is true, else panic.
//
// Depending on build arguments, DEBUG_ASSERT may or may not be enabled. When disabled, |x| will not
// be evaluated.
#define DEBUG_ASSERT(x)                       \
  do {                                        \
    if constexpr (DEBUG_ASSERT_IMPLEMENTED) { \
      if (x) [[likely]] {                     \
      } else [[unlikely]] {                   \
        assert_fail(__FILE__, __LINE__, #x);  \
      }                                       \
    }                                         \
  } while (0)

// Assert that |x| is true, else panic with the given message.
//
// Depending on build arguments, DEBUG_ASSERT_MSG may or may not be enabled. When disabled, |x| will
// not be evaluated.
#define DEBUG_ASSERT_MSG(x, msg, msgargs...)                     \
  do {                                                           \
    if constexpr (DEBUG_ASSERT_IMPLEMENTED) {                    \
      if (x) [[likely]] {                                        \
      } else [[unlikely]] {                                      \
        assert_fail_msg(__FILE__, __LINE__, #x, msg, ##msgargs); \
      }                                                          \
    }                                                            \
  } while (0)

#else  // __cplusplus

#define ASSERT(x)                          \
  do {                                     \
    if (unlikely(!(x))) {                  \
      assert_fail(__FILE__, __LINE__, #x); \
    }                                      \
  } while (0)

#define ASSERT_MSG(x, msg, msgargs...)                         \
  do {                                                         \
    if (unlikely(!(x))) {                                      \
      assert_fail_msg(__FILE__, __LINE__, #x, msg, ##msgargs); \
    }                                                          \
  } while (0)

#define DEBUG_ASSERT(x)                               \
  do {                                                \
    if (DEBUG_ASSERT_IMPLEMENTED && unlikely(!(x))) { \
      assert_fail(__FILE__, __LINE__, #x);            \
    }                                                 \
  } while (0)

#define DEBUG_ASSERT_MSG(x, msg, msgargs...)                   \
  do {                                                         \
    if (DEBUG_ASSERT_IMPLEMENTED && unlikely(!(x))) {          \
      assert_fail_msg(__FILE__, __LINE__, #x, msg, ##msgargs); \
    }                                                          \
  } while (0)

// make sure static_assert() is defined, even in C
#if !defined(static_assert)
#define static_assert(e, msg) _Static_assert(e, msg)
#endif

#endif  // __cplusplus

// implement _COND versions of DEBUG_ASSERT which only emit the body if
// DEBUG_ASSERT_IMPLEMENTED is set
#if DEBUG_ASSERT_IMPLEMENTED
#define DEBUG_ASSERT_COND(x) DEBUG_ASSERT(x)
#define DEBUG_ASSERT_MSG_COND(x, msg, msgargs...) DEBUG_ASSERT_MSG(x, msg, msgargs)
#else
#define DEBUG_ASSERT_COND(x) \
  do {                       \
  } while (0)
#define DEBUG_ASSERT_MSG_COND(x, msg, msgargs...) \
  do {                                            \
  } while (0)
#endif

// Use DEBUG_ASSERT or ASSERT instead.
//
// assert() is defined only because third-party code used in the kernel expects it.
#define assert(x) DEBUG_ASSERT(x)

__BEGIN_CDECLS

// The following functions are called when an assert fails.
void assert_fail(const char *file, int line, const char *expression) __NO_RETURN __NO_INLINE;
void assert_fail_msg(const char *file, int line, const char *expression, const char *fmt,
                     ...) __NO_RETURN __NO_INLINE __PRINTFLIKE(4, 5);

__END_CDECLS

#endif  // ZIRCON_KERNEL_INCLUDE_ASSERT_H_
