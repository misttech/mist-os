// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_RAII_PY_WRAPPER_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_RAII_PY_WRAPPER_H_
#include "src/developer/ffx/lib/fuchsia-controller/cpp/python/py_header.h"

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

}  // namespace py

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_RAII_PY_WRAPPER_H_
