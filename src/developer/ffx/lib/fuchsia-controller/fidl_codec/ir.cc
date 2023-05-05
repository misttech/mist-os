// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "ir.h"

#include <Python.h>

#include <fstream>
#include <iostream>
#include <sstream>
#include <string>

#include "mod.h"
#include "rapidjson/document.h"
#include "rapidjson/error/en.h"

namespace ir {

PyMethodDef add_ir_path_py_def = {
    "add_ir_path", reinterpret_cast<PyCFunction>(add_ir_path), METH_O,
    "Adds the FIDL IR path to the module for use in encoding/decoding. Throws an exception if the file cannot be parsed, or if the file does not exist."};

PyObject *add_ir_path(PyObject *self, PyObject *path_obj) {  // NOLINT
  const char *c_path = PyUnicode_AsUTF8(path_obj);
  if (c_path == nullptr) {
    return nullptr;
  }
  auto path = std::string(c_path);
  fidl_codec::LibraryReadError loader_err;
  mod::get_module_state()->loader->AddPath(path, &loader_err);
  switch (loader_err.value) {
    case fidl_codec::LibraryReadError::kIoError: {
      // Need a more specific error here. It appears this error is from being unable to find/open
      // the file, but if the FIDL IR depfile is correct this error shouldn't be hit (at least
      // not in-tree if the build is kept up to date).
      std::stringstream ss;
      ss << "Unable to load FIDL IR library at: " << path;
      PyErr_SetString(PyExc_RuntimeError, ss.str().c_str());
      return nullptr;
    }
    case fidl_codec::LibraryReadError::kParseError:
      PyErr_SetString(PyExc_RuntimeError,
                      rapidjson::GetParseError_En(loader_err.parse_result.Code()));
      return nullptr;
    case fidl_codec::LibraryReadError::kOk:
      break;
    default:
      PyErr_SetString(PyExc_RuntimeError, "Unhandled FIDL_codec return result");
      return nullptr;
  };
  Py_RETURN_NONE;
}

}  // namespace ir
