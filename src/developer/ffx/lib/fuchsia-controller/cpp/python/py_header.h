// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This is just a wrapper header for including Python that enforces the limited API has been set,
// to ensure ABI compatibility.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_PYTHON_PY_HEADER_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_PYTHON_PY_HEADER_H_

#define PY_SSIZE_T_CLEAN
#define Py_LIMITED_API 0x030b00f0
#include <Python.h>

#include <locale>
#include <sstream>
#include <string>
#include <unordered_set>

// Just convenience functions that ensures type-checking of the input being cast.
inline PyObject* PyObjCast(PyTypeObject* obj) { return reinterpret_cast<PyObject*>(obj); }
inline PyTypeObject* PyTypeCast(PyObject* obj) { return reinterpret_cast<PyTypeObject*>(obj); }

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
  if (python_keywords.count(lower_snake) > 0) {
    lower_snake.append("_");
  }
  return lower_snake;
}

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_PYTHON_PY_HEADER_H_
