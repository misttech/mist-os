// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_INTERNAL_DIAGNOSTICS_PRINTF_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_INTERNAL_DIAGNOSTICS_PRINTF_H_

#include <inttypes.h>

#include <string_view>
#include <tuple>
#include <type_traits>

#include "const-string.h"

namespace elfldltl {

template <typename T>
struct FileOffset;

template <typename T>
struct FileAddress;

class SymbolName;

namespace internal {

// This only exists to be specialized.  The interface is shown here.
template <typename T>
struct PrintfType {
  // The default template also handles other unsigned types that are the
  // same size as one of the uintNN_t types but a different type, which
  // just get widened to uint64_t.
  static_assert(std::is_unsigned_v<T>, "missing specialization");

  // This is a ConstString of a printf format string fragment.
  static constexpr auto kFormat = ConstString(" %" PRIu64);

  // This is a function of T that returns a std::tuple<...> of the arguments to
  // pass to printf corresponding to the kFormat string.
  static constexpr auto Arguments(uint64_t arg) { return std::make_tuple(arg); }
};

template <typename T>
struct PrintfType<T&> : public PrintfType<T> {};

template <typename T>
struct PrintfType<const T&> : public PrintfType<T> {};

template <typename T>
struct PrintfType<T&&> : public PrintfType<T> {};

template <>
struct PrintfType<uint8_t> {
  static constexpr auto kFormat = ConstString(" %" PRIu8);
  static constexpr auto Arguments(uint8_t arg) { return std::make_tuple(arg); }
};

template <>
struct PrintfType<uint16_t> {
  static constexpr auto kFormat = ConstString(" %" PRIu16);
  static constexpr auto Arguments(uint16_t arg) { return std::make_tuple(arg); }
};

template <>
struct PrintfType<uint32_t> {
  static constexpr auto kFormat = ConstString(" %" PRIu32);
  static constexpr auto Arguments(uint32_t arg) { return std::make_tuple(arg); }
};

template <typename T, size_t N>
struct Map {
  using Type = T;
  ConstString<N> string;
};

template <typename T, size_t N>
constexpr auto FormatFor(const char (&string)[N]) {
  return Map<T, N - 1>{string};
}

template <typename T, class First, class... Rest>
constexpr auto Pick(First first, Rest... rest) {
  if constexpr (std::is_same_v<T, typename First::Type>) {
    return first.string;
  } else {
    static_assert(sizeof...(Rest) > 0, "missing type?");
    return Pick<T>(rest...);
  }
}

template <typename T>
constexpr auto PrintfHexFormatStringForType() {
  return ConstString(" %#") + Pick<T>(                                  //
                                  FormatFor<uint8_t>(PRIx8),            //
                                  FormatFor<uint16_t>(PRIx16),          //
                                  FormatFor<uint32_t>(PRIx32),          //
                                  FormatFor<uint64_t>(PRIx64),          //
                                  FormatFor<unsigned char>("hhx"),      //
                                  FormatFor<unsigned short int>("hx"),  //
                                  FormatFor<unsigned int>("x"),         //
                                  FormatFor<unsigned long int>("lx"),   //
                                  FormatFor<unsigned long long int>("llx"));
}

// This handles string literals.  It could fold them into the format
// string, but that would require doubling any '%' inside.
template <size_t N>
struct PrintfType<const char (&)[N]> {
  static constexpr auto kFormat = ConstString("%s");
  static constexpr auto Arguments(const char (&str)[N]) { return std::forward_as_tuple(str); }
};

template <size_t Len>
struct PrintfType<ConstString<Len>> {
  static constexpr auto kFormat = ConstString("%s");
  static constexpr auto Arguments(const ConstString<Len>& str) {
    return std::make_tuple(str.c_str());
  }
};

template <>
struct PrintfType<const char*> {
  static constexpr auto kFormat = ConstString("%s");
  static constexpr auto Arguments(const char* str) { return std::make_tuple(str); }
};

template <>
struct PrintfType<std::string_view> {
  static constexpr auto kFormat = ConstString("%.*s");
  static constexpr auto Arguments(std::string_view str) {
    return std::make_tuple(static_cast<int>(str.size()), str.data());
  }
};

template <>
struct PrintfType<SymbolName> : public PrintfType<std::string_view> {};

template <typename T>
struct PrintfType<FileOffset<T>> {
  static constexpr auto kFormat =
      ConstString(" at file offset") + PrintfHexFormatStringForType<T>();
  static constexpr auto Arguments(FileOffset<T> arg) { return std::make_tuple(*arg); }
};

template <typename T>
struct PrintfType<FileAddress<T>> {
  static constexpr auto kFormat =
      ConstString(" at relative address") + PrintfHexFormatStringForType<T>();
  static constexpr auto Arguments(FileAddress<T> arg) { return std::make_tuple(*arg); }
};

// This concatenates them all together in a mandatory constexpr context so the
// whole format string becomes effectively a single string literal.
template <typename... T>
inline constexpr auto kPrintfFormat = (PrintfType<T>::kFormat + ...);

// Specialize the empty case.
template <>
inline constexpr auto kPrintfFormat<> = ConstString("");

// Calls printer("format string", ...) with arguments corresponding to
// prefix..., args... (each prefix argument and each later argument might
// produce multiple arguments to printer).
template <typename Printer, typename... Prefix, typename... Args>
constexpr void Printf(Printer&& printer, std::tuple<Prefix...> prefix, Args&&... args) {
  constexpr auto printer_args = [](auto&&... args) {
    constexpr auto kFormat = kPrintfFormat<decltype(args)...>.c_str();
    constexpr auto arg_tuple = [](auto&& arg) {
      using T = decltype(arg);
      return PrintfType<T>::Arguments(std::forward<T>(arg));
    };
    return std::tuple_cat(std::make_tuple(kFormat),
                          arg_tuple(std::forward<decltype(args)>(args))...);
  };
  std::apply(
      std::forward<Printer>(printer),
      std::apply(printer_args, std::tuple_cat(std::move(prefix),
                                              std::forward_as_tuple(std::forward<Args>(args)...))));
}

}  // namespace internal
}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_INTERNAL_DIAGNOSTICS_PRINTF_H_
