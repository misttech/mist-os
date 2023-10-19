// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "handle.h"

#include "convert.h"
#include "fuchsia_controller.h"
#include "mod.h"

namespace handle {

void Handle_dealloc(Handle *self) {
  ffx_close_handle(self->handle);
  PyObject_Free(self);
}

int Handle_init(Handle *self, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"handle", nullptr};
  zx_handle_t handle;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "I", const_cast<char **>(kwlist), &handle)) {
    return -1;
  }
  if (handle == convert::MINUS_ONE_U32 && PyErr_Occurred()) {
    return -1;
  }
  self->handle = handle;
  return 0;
}

PyObject *Handle_as_int(Handle *self, PyObject *Py_UNUSED(arg)) {
  return PyLong_FromUnsignedLongLong(self->handle);
}

PyObject *Handle_take(Handle *self, PyObject *Py_UNUSED(arg)) {
  auto result = PyLong_FromUnsignedLongLong(self->handle);
  self->handle = 0;
  return result;
}

PyObject *Handle_close(Handle *self, PyObject *Py_UNUSED(arg)) {
  ffx_close_handle(self->handle);
  self->handle = 0;
  Py_RETURN_NONE;
}

PyMethodDef Handle_methods[] = {
    {"as_int", reinterpret_cast<PyCFunction>(Handle_as_int), METH_NOARGS, nullptr},
    {"take", reinterpret_cast<PyCFunction>(Handle_take), METH_NOARGS,
     "Takes the underlying fidl handle, setting it internally to zero (thus invalidating the "
     "underlying channel). This is used for sending a handle through FIDL function calls."},
    {"close", reinterpret_cast<PyCFunction>(Handle_close), METH_NOARGS,
     "Closes the underlying handle. This will invalidate any other copies of this channel."},
    {nullptr, nullptr, 0, nullptr}};

PyType_Slot Handle_slots[] = {
    {Py_tp_dealloc, reinterpret_cast<void *>(Handle_dealloc)},
    {Py_tp_doc,
     reinterpret_cast<void *>(const_cast<char *>(
         "Fuchsia controller FIDL handle. This is used to bootstrap processes for FIDL interactions."))},
    {Py_tp_init, reinterpret_cast<void *>(Handle_init)},
    {Py_tp_methods, reinterpret_cast<void *>(Handle_methods)},
    {0, nullptr},
};

PyType_Spec HandleType_Spec = {
    .name = "fuchsia_controller_py.Handle",
    .basicsize = sizeof(Handle),
    .slots = Handle_slots,
};

PyTypeObject *HandleType = nullptr;

int HandleTypeInit() { return mod::GenericTypeInit(&HandleType, &HandleType_Spec); }

}  // namespace handle
