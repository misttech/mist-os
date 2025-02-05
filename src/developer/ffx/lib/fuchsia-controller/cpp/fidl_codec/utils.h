// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FIDL_CODEC_UTILS_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FIDL_CODEC_UTILS_H_

#include <Python.h>

#include <locale>
#include <sstream>
#include <string>
#include <unordered_set>

namespace fuchsia_controller::fidl_codec::utils {

// Converts camel case to lower snake case. Does not handle acronyms.
inline std::string ToLowerSnake(std::string_view s) {
  std::stringstream ss;
  auto iter = s.cbegin();
  ss.put(std::tolower(*iter, std::locale()));
  iter++;
  for (; iter != s.cend(); ++iter) {
    auto c = *iter;
    if (std::isupper(c)) {
      ss.put('_');
    }
    ss.put(std::tolower(c, std::locale()));
  }
  return ss.str();
}

// This is a recreation of the "normalize_member_name" function from Python.
inline std::string NormalizeMemberName(std::string_view s) {
  // These keywords are taken from the Python docs:
  // https://docs.python.org/3/reference/lexical_analysis.html#keywords
  // Uppercase keywords have been omitted.
  static std::unordered_set<std::string> python_keywords = {
      "await",  "else",    "import", "pass",   "break",    "except",   "in",     "raise",
      "class",  "finally", "is",     "return", "and",      "continue", "for",    "lambda",
      "try",    "as",      "def",    "from",   "nonlocal", "while",    "assert", "del",
      "global", "not",     "with",   "async",  "elif",     "if",       "or",     "yield"};
  auto lower_snake = ToLowerSnake(s);
  if (python_keywords.contains(lower_snake)) {
    lower_snake.append("_");
  }
  return lower_snake;
}

static constexpr uint32_t MINUS_ONE_U32 = std::numeric_limits<uint32_t>::max();
static constexpr uint64_t MINUS_ONE_U64 = std::numeric_limits<uint64_t>::max();
static_assert(sizeof(unsigned long long) == sizeof(uint64_t));  // NOLINT

inline uint32_t PyLong_AsU32(PyObject* py_long) {
  auto res = PyLong_AsUnsignedLongLong(py_long);
  if (res > static_cast<uint64_t>(MINUS_ONE_U32)) {
    PyErr_Format(PyExc_OverflowError, "Value %" PRIu64 " too large for u32", res);
    return MINUS_ONE_U32;
  }
  return static_cast<uint32_t>(res);
}

inline uint64_t PyLong_AsU64(PyObject* py_long) {
  return static_cast<uint64_t>(PyLong_AsUnsignedLongLong(py_long));
}

class Buffer {
 public:
  explicit Buffer(Py_buffer buf) : buffer_(buf) {}
  ~Buffer() { PyBuffer_Release(&buffer_); }
  Py_ssize_t len() const { return buffer_.len; }
  void* buf() const { return buffer_.buf; }

  Buffer(const Buffer&) = delete;
  Buffer& operator=(const Buffer&) = delete;
  Buffer(Buffer&&) = delete;
  Buffer& operator=(Buffer&&) = delete;

 private:
  Py_buffer buffer_;
};

}  // namespace fuchsia_controller::fidl_codec::utils

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FIDL_CODEC_UTILS_H_
