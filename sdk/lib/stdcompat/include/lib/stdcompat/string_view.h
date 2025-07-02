// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_STDCOMPAT_STRING_VIEW_H_
#define LIB_STDCOMPAT_STRING_VIEW_H_

#include <cstddef>
#include <stdexcept>
#include <string_view>

#include "version.h"

// Per the README, we define standalone cpp20::starts_with() and
// cpp20::ends_with() functions. These correspond to std::basic_string_view
// methods introduced in C++20. For parity's sake, in the C++20 context we also
// define the same functions, though as thin wrappers around these methods.
namespace cpp20 {

#if defined(__cpp_lib_string_view) && __cpp_lib_string_view >= 202002L && \
    !defined(LIB_STDCOMPAT_USE_POLYFILLS)

template <class CharT, class Traits = std::char_traits<CharT>, typename PrefixType>
constexpr bool starts_with(std::basic_string_view<CharT, Traits> s,
                           std::decay_t<PrefixType> prefix) {
  return s.starts_with(prefix);
}

template <class CharT, class Traits = std::char_traits<CharT>, typename SuffixType>
constexpr bool ends_with(std::basic_string_view<CharT, Traits> s, std::decay_t<SuffixType> suffix) {
  return s.ends_with(suffix);
}

#else  // Polyfills for C++20 std::basic_string_view methods.

template <class CharT, class Traits = std::char_traits<CharT>>
constexpr bool starts_with(std::basic_string_view<CharT, Traits> s, decltype(s) prefix) {
  return s.substr(0, prefix.size()) == prefix;
}

template <class CharT, class Traits = std::char_traits<CharT>>
constexpr bool starts_with(std::basic_string_view<CharT, Traits> s, const CharT* prefix) {
  return starts_with(s, decltype(s){prefix});
}

template <class CharT, class Traits = std::char_traits<CharT>>
constexpr bool starts_with(std::basic_string_view<CharT, Traits> s, CharT c) {
  return !s.empty() && Traits::eq(s.front(), c);
}

template <class CharT, class Traits = std::char_traits<CharT>>
constexpr bool ends_with(std::basic_string_view<CharT, Traits> s, decltype(s) suffix) {
  return s.size() >= suffix.size() && s.substr(s.size() - suffix.size(), suffix.size()) == suffix;
}

template <class CharT, class Traits = std::char_traits<CharT>>
constexpr bool ends_with(std::basic_string_view<CharT, Traits> s, const CharT* suffix) {
  return ends_with(s, decltype(s){suffix});
}

template <class CharT, class Traits = std::char_traits<CharT>>
constexpr bool ends_with(std::basic_string_view<CharT, Traits> s, CharT c) {
  return !s.empty() && Traits::eq(s.back(), c);
}

#endif  // if __cpp_lib_string_view >= 202002L && !defined(LIB_STDCOMPAT_USE_POLYFILLS)

}  // namespace cpp20

#endif  // LIB_STDCOMPAT_STRING_VIEW_H_
