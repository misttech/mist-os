// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "efi_variables.h"

#include <inttypes.h>
#include <xefi.h>

#include <string>

#include <fbl/vector.h>
#include <phys/efi/main.h>

#include "src/lib/utf_conversion/utf_conversion.h"

namespace fbl {
template <typename T>
inline bool operator==(const fbl::Vector<T>& a, const fbl::Vector<T>& b) {
  return std::equal(a.begin(), a.end(), b.begin(), b.end());
}

template <typename T>
inline bool operator!=(const fbl::Vector<T>& a, const fbl::Vector<T>& b) {
  return !(a == b);
}
}  // namespace fbl

namespace gigaboot {

EfiVariables::EfiVariableId::EfiVariableId(const EfiVariableId& src)
    : EfiVariableId(src.name, src.vendor_guid) {}

EfiVariables::EfiVariableId::EfiVariableId(const fbl::Vector<char16_t>& name_in,
                                           const efi_guid& vendor_guid_in)
    : vendor_guid(vendor_guid_in) {
  name.resize(name_in.size());
  std::copy(name_in.begin(), name_in.end(), name.begin());
}

EfiVariables::EfiVariableId& EfiVariables::EfiVariableId::operator=(const EfiVariableId& src) {
  vendor_guid = src.vendor_guid;
  name.resize(src.name.size());
  std::copy(src.name.begin(), src.name.end(), name.begin());
  return *this;
}

bool EfiVariables::EfiVariableId::IsValid() const {
  return name != fbl::Vector<char16_t>({kPreBeginVarNameInit});
}

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

fit::result<efi_status, fbl::Vector<char>> EfiVariables::Ucs2ToStr(
    const fbl::Vector<char16_t>& ucs2) {
  return Ucs2ToStr(ToU16StringView(ucs2));
}

fit::result<efi_status, fbl::Vector<char16_t>> EfiVariables::StrToUcs2(std::string_view str) {
  size_t dst_len = str.size() + 1;
  fbl::Vector<char16_t> out;
  out.resize(dst_len, u'\0');

  if (str.empty()) {
    return fit::success(fbl::Vector<char16_t>{u'\0'});
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

fit::result<efi_status, fbl::Vector<char16_t>> EfiVariables::StrToUcs2(
    const fbl::Vector<char>& str) {
  const std::string_view sv(str.data(), str.size());
  return StrToUcs2(sv);
}

fit::result<efi_status, fbl::Vector<uint8_t>> EfiVariables::EfiGetVariable(
    const EfiVariables::EfiVariableId& v_id) const {
  fbl::Vector<uint8_t> res;

  // Get variable length first
  size_t variable_size = 0;
  efi_guid vendor_guid = v_id.vendor_guid;
  efi_status status = gEfiSystemTable->RuntimeServices->GetVariable(
      const_cast<char16_t*>(v_id.name.data()), &vendor_guid, NULL, &variable_size, nullptr);

  // Get buffer of the correct size and read into it
  if (EFI_BUFFER_TOO_SMALL == status) {
    res.resize(variable_size);

    status = gEfiSystemTable->RuntimeServices->GetVariable(
        const_cast<char16_t*>(v_id.name.data()), &vendor_guid, NULL, &variable_size, res.data());
  }

  if (EFI_ERROR(status)) {
    return fit::error(status);
  }

  return fit::success(std::move(res));
}

fit::result<efi_status, efi_guid> EfiVariables::GetGuid(std::u16string_view var_name) {
  auto IsNameMatch = [&var_name](const EfiVariables::EfiVariableId& variable_id) {
    return std::equal(variable_id.name.begin(), variable_id.name.end(), var_name.begin(),
                      var_name.end());
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

fit::result<efi_status, efi_guid> EfiVariables::GetGuid(const fbl::Vector<char16_t>& var_name) {
  return GetGuid(ToU16StringView(var_name));
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

fbl::Vector<char16_t> ToVector(std::u16string_view str) {
  fbl::Vector<char16_t> res;
  res.resize(str.size());
  std::copy(str.begin(), str.end(), res.begin());
  return res;
}

fbl::Vector<char> ToVector(std::string_view str) {
  fbl::Vector<char> res;
  res.resize(str.size());
  std::copy(str.begin(), str.end(), res.begin());
  return res;
}

}  // namespace gigaboot
