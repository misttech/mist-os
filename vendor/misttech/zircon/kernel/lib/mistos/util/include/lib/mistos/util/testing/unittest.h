

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TESTING_UNITTEST_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TESTING_UNITTEST_H_

#include <lib/unittest/unittest.h>

inline std::string_view ToStringView(std::string_view str) { return str; }

// We avoid calling std::string_view's constructor of a single `const char*`
// argument, as that will call std::char_traits<char>::length() which may not
// support nullptrs.
inline std::string_view ToStringView(const char* str) {
  if (!str) {
    return {};
  }
  return {str, strlen(str)};
}

inline std::string_view ToStringView(char* str) {
  return ToStringView(static_cast<const char*>(str));
}

// Else, default to assuming that std::data will yield a C string.
template <typename Stringlike>
inline std::string_view ToStringView(const Stringlike& str)
  requires(!std::is_convertible_v<Stringlike, nullptr_t>)
{
  static_assert(
      std::is_convertible_v<decltype(std::data(std::declval<Stringlike&>())), const char*>);
  return ToStringView(std::data(str));
}

template <typename Stringlike>
inline std::string_view ToStringView(const Stringlike& str)
  requires(std::is_convertible_v<Stringlike, nullptr_t>)
{
  return {};
}

template <typename StringTypeA, typename StringTypeB>
inline bool StrCmp(StringTypeA&& actual, StringTypeB&& expected) {
  std::string_view actual_sv = ToStringView(actual);
  std::string_view expected_sv = ToStringView(expected);
  return actual_sv == expected_sv;
}

#define EXPECT_STREQ(expected, actual, ...) EXPECT_TRUE(StrCmp(actual, expected), ##__VA_ARGS__);
#define ASSERT_STREQ(expected, actual, ...) ASSERT_TRUE(StrCmp(actual, expected), ##__VA_ARGS__);

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TESTING_UNITTEST_H_
