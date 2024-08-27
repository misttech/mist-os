// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBITL_INCLUDE_LIB_MISTOS_ZBITL_ERROR_STRING_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZBITL_INCLUDE_LIB_MISTOS_ZBITL_ERROR_STRING_H_

#include <fbl/string.h>

// The format of the error strings below should be kept in sync with that of
// the printed messages in error_stdio.h.

namespace zbitl {

// Returns an error string from a View `Error` value.
template <typename ViewError>
fbl::String ViewErrorString(const ViewError& error) {
  char digits[6];
  fbl::String s{error.zbi_error};
  s += " at offset ";
  snprintf(digits, sizeof(digits), "%d", error.item_offset);
  s += digits;
  if (error.storage_error) {
    s += ": ";
    s += ViewError::storage_error_string(error.storage_error.value());
  }
  return s;
}

// Returns an error string from a View `CopyError` value.
template <typename ViewCopyError>
fbl::String ViewCopyErrorString(const ViewCopyError& error) {
  char digits[6];
  fbl::String s{error.zbi_error};
  if (error.read_error) {
    s += ": read error at source offset ";
    snprintf(digits, sizeof(digits), "%d", error.read_offset);
    s += digits;
    s += ": ";
    s += ViewCopyError::read_error_string(error.read_error.value());
  } else if (error.write_error) {
    s += ": write error at destination offset ";
    snprintf(digits, sizeof(digits), "%d", error.write_offset);
    s += digits;
    s += ": ";
    s += ViewCopyError::write_error_string(error.write_error.value());
  }
  return s;
}

// Returns an error string from a Bootfs `Error` value.
template <typename BootfsError>
fbl::String BootfsErrorString(const BootfsError& error) {
  char digits[6];
  fbl::String s{error.reason};

  if (error.entry_offset > 0) {
    s += ": at dirent offset ";
    snprintf(digits, sizeof(digits), "%d", error.entry_offset);
    s += digits;
  }
  if (!error.filename.empty()) {
    s += ": with filename \"";
    s += error.filename;
    s += "\"";
  }
  if (error.storage_error) {
    s += ": ";
    s += BootfsError::storage_error_string(error.storage_error.value());
  }
  return s;
}

}  // namespace zbitl

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBITL_INCLUDE_LIB_MISTOS_ZBITL_ERROR_STRING_H_
