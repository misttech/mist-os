// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "isolate_directory.h"

#include <cerrno>
#include <cstdlib>
#include <filesystem>
#include <string>
#include <system_error>

#include "mod.h"

namespace isolate {

void IsolateDir_dealloc(IsolateDir *self) {
  // If the directory is never successfully created on sending it to a context, then
  // nothing happens.
  if (std::filesystem::is_directory(self->dir)) {
    std::filesystem::remove_all(self->dir);
  }
}

int IsolateDir_init(IsolateDir *self, PyObject *args, PyObject *kwds) {  // NOLINT
  static const char *kwlist[] = {"dir", nullptr};
  const char *dir_cstr = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "|z", const_cast<char **>(kwlist), &dir_cstr)) {
    return -1;
  }

  // If the user didn't specify their own directory, randomly generate a temporary one
  const char *error_format_str = "Error when creating isolate directory %s: %s";
  if (dir_cstr == nullptr) {
    std::filesystem::path tmp_dir_path = std::filesystem::temp_directory_path() / "fctemp.XXXXXX";

    // Guarantee the temporary directory is created.
    // mkdtemp modifies its parameter in-place, so use it to create a
    // filesystem::path before it goes out of scope
    std::string tmp_dir_str = tmp_dir_path.string();
    if (mkdtemp(tmp_dir_str.data()) == nullptr) {
      const char *error_str = strerror(errno);
      PyErr_Format(PyExc_IOError, error_format_str, tmp_dir_str.c_str(), error_str);
      return -1;
    }
    self->dir = std::filesystem::path(tmp_dir_str);
  } else {
    std::filesystem::path tmp_path = std::filesystem::path(dir_cstr);

    // Guarantee the directory is created.
    std::error_code err;
    if (!std::filesystem::create_directory(tmp_path, err)) {
      // If |err| is falsey this indicates success, although `create_directory`
      // might return false.  This can occur when the directory already exists,
      // so creating it succeeds even though there was an "error".
      //
      // Don't raise a python error in this case.
      if (err) {
        PyErr_Format(PyExc_IOError, error_format_str, tmp_path.c_str(), err.message().c_str());
        return -1;
      }
    }
    self->dir = std::move(tmp_path);
  }

  return 0;
}

PyObject *IsolateDir_directory(IsolateDir *self, PyObject *Py_UNUSED(arg)) {
  return PyUnicode_FromString(self->dir.c_str());
}

PyMethodDef IsolateDir_methods[] = {
    {"directory", reinterpret_cast<PyCFunction>(IsolateDir_directory), METH_NOARGS,
     "Returns a string representing the directory to which this IsolateDir points. The IsolateDir will create it upon initialization."},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject IsolateDirType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.IsolateDir",
    .tp_basicsize = sizeof(IsolateDir),
    .tp_itemsize = 0,
    .tp_dealloc = reinterpret_cast<destructor>(IsolateDir_dealloc),
    .tp_doc =
        "Fuchsia controller Isolate Directory. Represents an Isolate Directory path to be used by the fuchsia controller Context object. This object cleans up the Isolate Directory (if it exists) once it goes out of scope.",
    .tp_methods = IsolateDir_methods,
    .tp_init = reinterpret_cast<initproc>(IsolateDir_init),
    .tp_new = PyType_GenericNew,
};

}  // namespace isolate
