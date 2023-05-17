// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "ir.h"

#include <Python.h>

#include <fstream>
#include <iostream>
#include <sstream>

#include "mod.h"
#include "pyerrors.h"
#include "rapidjson/document.h"
#include "rapidjson/error/en.h"

namespace ir {

PyMethodDef add_ir_path_py_def = {
    "add_ir_path", reinterpret_cast<PyCFunction>(add_ir_path), METH_O,
    "Adds the FIDL IR path to the module for use in encoding/decoding. Throws an exception if the "
    "file cannot be parsed, or if the file does not exist."};

PyMethodDef get_method_ordinal_py_def = {
    "method_ordinal", reinterpret_cast<PyCFunction>(get_method_ordinal),
    METH_VARARGS | METH_KEYWORDS,
    "Gets the method ordinal number for the specified method. Method is intended to be a string "
    "formatted as a FIDL fully qualified name, e.g. '<library>/<declaration>.<member>'. For "
    "example: 'fuchsia.developer.ffx/EchoString'. More details can be found at "
    "https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0043_documentation_comment_format#fully-qualified-names"};

PyObject *get_method_ordinal(PyObject *self, PyObject *args, PyObject *kwds) {  // NOLINT
  static const char *kwlist[] = {"protocol", "method", nullptr};
  const char *c_protocol;
  const char *c_method;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "ss", const_cast<char **>(kwlist), &c_protocol,
                                   &c_method)) {
    return nullptr;
  }
  std::string_view protocol(c_protocol);
  auto const idx = protocol.find('/');
  if (idx == std::string::npos) {
    PyErr_SetString(PyExc_ValueError,
                    "protocol not formatted properly, expected {library}/{protocol}");
    return nullptr;
  }
  const std::string library(c_protocol, idx);
  std::string_view method(c_method);
  auto lib = mod::get_ir_library(library);
  if (lib == nullptr) {
    return nullptr;
  }
  fidl_codec::Protocol *codec_protocol;
  if (!lib->GetProtocolByName(protocol, &codec_protocol)) {
    std::stringstream ss;
    ss << "Unable to find protocol " << protocol << " under library " << library;
    PyErr_SetString(PyExc_RuntimeError, ss.str().c_str());
    return nullptr;
  }
  auto codec_method = codec_protocol->GetMethodByName(method);
  if (codec_method == nullptr) {
    std::stringstream ss;
    ss << "Unable to find method " << method << " under protocol " << protocol << " in library "
       << library;
    PyErr_SetString(PyExc_RuntimeError, ss.str().c_str());
    return nullptr;
  }
  return PyLong_FromUnsignedLongLong(codec_method->ordinal());
}

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
