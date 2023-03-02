// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "efi_variables.h"

#include <inttypes.h>
#include <xefi.h>

#include <string>

#include <phys/efi/main.h>

#include "src/lib/utf_conversion/utf_conversion.h"

namespace gigaboot {

fit::result<efi_status, EfiVariables::EfiVariableInfo> EfiVariables::EfiQueryVariableInfo() const {
  efi_status status;
  EfiVariableInfo efi_var_info = {};

  status = gEfiSystemTable->RuntimeServices->QueryVariableInfo(
      EFI_VARIABLE_BOOTSERVICE_ACCESS | EFI_VARIABLE_RUNTIME_ACCESS,
      &efi_var_info.max_var_storage_size, &efi_var_info.remaining_var_storage_size,
      &efi_var_info.max_var_size);
  if (EFI_ERROR(status)) {
    return fit::error(status);
  }

  return fit::success(efi_var_info);
}

fit::result<efi_status> EfiVariables::EfiGetNextVariableName(
    EfiVariables::EfiVariableId& variable_id) const {
  for (size_t i = 0; i < 2; i++) {
    // Get next variable name
    size_t variable_name_size = variable_id.name.size() * sizeof(variable_id.name[0]);
    efi_status status = gEfiSystemTable->RuntimeServices->GetNextVariableName(
        &variable_name_size, variable_id.name.data(), &variable_id.vendor_guid);
    if (EFI_BUFFER_TOO_SMALL == status) {
      variable_id.name.resize(variable_name_size / sizeof(variable_id.name[0]));
      continue;
    }
    if (EFI_ERROR(status)) {
      return fit::error(status);
    }

    variable_id.name.resize(variable_name_size / sizeof(variable_id.name[0]));
    return fit::ok();
  }

  return fit::error(EFI_ABORTED);
}

fit::result<efi_status, fbl::Vector<char>> EfiVariables::Ucs2ToStr(std::u16string_view ucs2) {
  size_t dst_len = 0;

  size_t variable_name_length = ucs2.size();
  if (variable_name_length == 0) {
    return fit::error(EFI_INVALID_PARAMETER);
  }

  // Don't count terminating '\0' character.
  --variable_name_length;
  if (ucs2[variable_name_length] != u'\0') {
    return fit::error(EFI_INVALID_PARAMETER);
  }

  if (variable_name_length == 0) {
    return fit::success(fbl::Vector<char>({'\0'}));
  }

  // Get required length for dst buffer
  zx_status_t res = utf16_to_utf8(reinterpret_cast<const uint16_t*>(ucs2.data()),
                                  variable_name_length, nullptr, &dst_len);
  if (res != ZX_OK) {
    return fit::error(EFI_INVALID_PARAMETER);
  }

  fbl::Vector<char> out;
  out.resize(dst_len + 1);
  res = utf16_to_utf8(reinterpret_cast<const uint16_t*>(ucs2.data()), variable_name_length,
                      reinterpret_cast<uint8_t*>(out.data()), &dst_len);
  if (res != ZX_OK) {
    return fit::error(EFI_INVALID_PARAMETER);
  }
  out[dst_len] = '\0';

  return fit::success(std::move(out));
}

fit::result<efi_status, std::u16string> EfiVariables::StrToUcs2(std::string_view str) {
  size_t dst_len = str.size() + 1;
  std::u16string out(dst_len, u'\0');

  if (str.length() == 0) {
    return fit::success(std::u16string(u"", 1));
  }

  // Get required length for dst buffer
  zx_status_t res = utf8_to_utf16(reinterpret_cast<const uint8_t*>(str.data()), str.size(),
                                  reinterpret_cast<uint16_t*>(out.data()), &dst_len);
  if (res != ZX_OK) {
    return fit::error(EFI_INVALID_PARAMETER);
  }

  if (dst_len != str.size()) {
    printf("UTF8 to UCS-2 convertion produced unexpected length: %zu (%zu expected)\n", dst_len,
           str.size());
  }

  return fit::success(std::move(out));
}

fit::result<efi_status, fbl::Vector<uint8_t>> EfiVariables::EfiGetVariable(
    const EfiVariables::EfiVariableId& v_id) const {
  fbl::Vector<uint8_t> res;

  // Get variable length first
  size_t variable_size = 0;
  efi_guid vendor_guid = v_id.vendor_guid;
  efi_status status = gEfiSystemTable->RuntimeServices->GetVariable(
      const_cast<char16_t*>(v_id.name.c_str()), &vendor_guid, NULL, &variable_size, nullptr);

  // Get buffer of the correct size and read into it
  if (EFI_BUFFER_TOO_SMALL == status) {
    res.resize(variable_size);

    status = gEfiSystemTable->RuntimeServices->GetVariable(
        const_cast<char16_t*>(v_id.name.c_str()), &vendor_guid, NULL, &variable_size, res.data());
  }

  if (EFI_ERROR(status)) {
    return fit::error(status);
  }

  return fit::success(std::move(res));
}

fit::result<efi_status, efi_guid> EfiVariables::GetGuid(std::u16string_view var_name) {
  auto IsNameMatch = [&var_name](const EfiVariables::EfiVariableId& variable_id) {
    return variable_id.name == var_name;
  };

  auto res = std::find_if(begin(), end(), IsNameMatch);
  if (res == end()) {
    if (res.has_failed_) {
      return fit::error(EFI_ABORTED);
    } else {
      return fit::error(EFI_NOT_FOUND);
    }
  }

  // Check if there is another variable with the same name
  auto res2 = res;
  res2 = std::find_if(std::next(res2), end(), IsNameMatch);
  if (res2 != end()) {
    return fit::error(EFI_INVALID_PARAMETER);
  }

  return fit::success(res.variable_id_.vendor_guid);
}

bool operator==(const EfiVariables::EfiVariableId& a, const EfiVariables::EfiVariableId& b) {
  return a.name == b.name && a.vendor_guid == b.vendor_guid;
}
bool operator!=(const EfiVariables::EfiVariableId& a, const EfiVariables::EfiVariableId& b) {
  return !(a == b);
}

bool EfiVariables::iterator::operator==(const iterator& other) const {
  return (is_end_ == other.is_end_) && (is_end_ || variable_id_ == other.variable_id_);
}

bool EfiVariables::iterator::operator!=(const iterator& other) const { return !(*this == other); }

EfiVariables::iterator& EfiVariables::iterator::operator++() {  // prefix
  if (is_end_) {
    return *this;
  }

  if (has_failed_) {
    is_end_ = true;
    return *this;
  }

  auto v_id = container_->EfiGetNextVariableName(variable_id_);
  if (v_id.is_error()) {
    if (v_id.error_value() == EFI_NOT_FOUND) {
      is_end_ = true;
    } else {
      has_failed_ = true;
      variable_id_.Invalidate();
      printf("Failed to get variable name from EfiGetNextVariableName(): %s",
             xefi_strerror(v_id.error_value()));
    }
  }
  return *this;
}

EfiVariables::iterator EfiVariables::iterator::operator++(int) {  // postfix
  iterator old = *this;
  ++*this;
  return old;
}

bool EfiVariables::EfiVariableInfo::operator==(
    const EfiVariables::EfiVariableInfo& other) const noexcept {
  return max_var_storage_size == other.max_var_storage_size &&
         remaining_var_storage_size == other.remaining_var_storage_size &&
         max_var_size == other.max_var_size;
}

bool EfiVariables::EfiVariableInfo::operator!=(const EfiVariableInfo& other) const noexcept {
  return !(*this == other);
}

}  // namespace gigaboot
