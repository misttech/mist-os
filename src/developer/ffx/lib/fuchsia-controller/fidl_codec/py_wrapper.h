// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_PY_WRAPPER_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_PY_WRAPPER_H_
#include <Python.h>

namespace py {

// Simple RAII wrapper for PyObject* types.
class Object {
 public:
  explicit Object(PyObject* ptr) : ptr_(ptr) {}
  ~Object() { Py_XDECREF(ptr_); }
  PyObject* get() { return ptr_; }
  PyObject* take() {
    auto res = ptr_;
    ptr_ = nullptr;
    return res;
  }

  // Convenience method for comparing to other pointers.
  bool operator==(PyObject* other) { return other == ptr_; }
  bool operator==(nullptr_t other) { return other == ptr_; }

 private:
  PyObject* ptr_;
};

}  // namespace py

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_PY_WRAPPER_H_
