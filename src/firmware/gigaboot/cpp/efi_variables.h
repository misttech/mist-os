// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_GIGABOOT_CPP_EFI_VARIABLES_H_
#define SRC_FIRMWARE_GIGABOOT_CPP_EFI_VARIABLES_H_

#include <lib/fit/result.h>
#include <lib/stdcompat/span.h>
#include <stdio.h>

#include <compare>
#include <initializer_list>
#include <iterator>
#include <string_view>
#include <variant>

#include <efi/types.h>
#include <fbl/vector.h>

namespace gigaboot {

inline std::u16string_view ToU16StringView(const fbl::Vector<char16_t>& v) {
  return std::u16string_view(v.data(), v.size());
}

// Helper wrapper class for EFI C API
//
// Contains EFI variables related logic at the moment.
// More convenient interface for following functions:
//  EfiGetNextVariableName()
//  EfiGetVariable()
//  QueryVariableInfo()
//
// Provides C++ style container wrapper for VariableName iteration
//
// E.g:
//   EfiVariables efi_variables;
//   for (const auto& v_id : efi_variables) {
//     if (!v_id.IsValid()) {
//       return false;
//     }
//
//     // do something with v_id
//   }
class EfiVariables {
 public:
  virtual ~EfiVariables() = default;

  struct EfiVariableId {
    fbl::Vector<char16_t> name{kPreBeginVarNameInit};
    efi_guid vendor_guid;

    EfiVariableId() = default;
    EfiVariableId(const fbl::Vector<char16_t>& name_in, const efi_guid& vendor_guid_in);
    EfiVariableId(const EfiVariableId& src);
    EfiVariableId& operator=(const EfiVariableId& src);

    // Special constant for `EfiGetNextVariableName()` to start from the beginning.
    // Also used to indicate invalid value, since it is not allowed as variable name.
    // constexpr static std::initializer_list<char16_t> kPreBeginVarNameInit{u'\0'};
    constexpr static char16_t kPreBeginVarNameInit = u'\0';

    // EFI API calls can fail. This method allows to check if iterator contains valid data
    // or is invalid (in case of failure).
    bool IsValid() const;

    // Marks iterator as invalid in case of error.
    void Invalidate() { name = {kPreBeginVarNameInit}; }
  };

  // UCS2 -> UTF8 string converter
  // ucs2 is expected to be a c-string
  static fit::result<efi_status, fbl::Vector<char>> Ucs2ToStr(std::u16string_view ucs2);
  static fit::result<efi_status, fbl::Vector<char>> Ucs2ToStr(const fbl::Vector<char16_t>& ucs2);
  // Basic UTF8 (ASCII) -> UCS2 string converter
  static fit::result<efi_status, fbl::Vector<char16_t>> StrToUcs2(std::string_view str);
  static fit::result<efi_status, fbl::Vector<char16_t>> StrToUcs2(const fbl::Vector<char>& str);

  struct EfiVariableInfo {
    uint64_t max_var_storage_size;
    uint64_t remaining_var_storage_size;
    uint64_t max_var_size;

    bool operator==(const EfiVariableInfo&) const noexcept;
    bool operator!=(const EfiVariableInfo&) const noexcept;
  };
  virtual fit::result<efi_status, EfiVariableInfo> EfiQueryVariableInfo() const;
  virtual fit::result<efi_status, fbl::Vector<uint8_t>> EfiGetVariable(
      const EfiVariableId& vn) const;

  // Search for GUID for VariableName
  //
  // Returns:
  // `efi_guid` on success
  // or
  // EFI_NOT_FOUND if there are no Variables with provided name were found
  // EFI_ABORTED on any other error
  // EFI_INVALID_PARAMETER if there were multiple variables with the same name were found
  virtual fit::result<efi_status, efi_guid> GetGuid(std::u16string_view var_name);
  fit::result<efi_status, efi_guid> GetGuid(const fbl::Vector<char16_t>& var_name);

  class iterator {
    friend EfiVariables;

   public:
    using value_type = EfiVariableId;
    using reference = value_type&;
    using pointer = value_type*;
    using difference_type = std::size_t;
    using iterator_category = std::forward_iterator_tag;

    explicit iterator(EfiVariables* container) : container_(container) {}

    value_type operator*() const { return variable_id_; }
    value_type* operator->() { return &variable_id_; }

    bool operator==(const iterator& other) const;
    bool operator!=(const iterator& other) const;

    iterator& operator++();
    iterator operator++(int);

   private:
    value_type variable_id_;

    // Flag for end() iterator
    bool is_end_ = false;
    // Flag for tracking errors while iterating
    bool has_failed_ = false;
    // Parent container reference
    EfiVariables* container_;
  };

  iterator begin() {
    iterator it(this);
    return ++it;
  }

  iterator end() {
    iterator it(this);
    it.is_end_ = true;
    return it;
  }

 private:
  // Provides next `variable_id` that goes after input one.
  // If there are no more variables left returns EFI_NOT_FOUND.
  // Making it virtual to make it mockable for tests.
  virtual fit::result<efi_status> EfiGetNextVariableName(
      EfiVariables::EfiVariableId& variable_id) const;
};

bool operator==(const EfiVariables::EfiVariableId& a, const EfiVariables::EfiVariableId& b);
bool operator!=(const EfiVariables::EfiVariableId& a, const EfiVariables::EfiVariableId& b);

fbl::Vector<char> ToVector(std::string_view str);
fbl::Vector<char16_t> ToVector(std::u16string_view str);

}  // namespace gigaboot

#endif  // SRC_FIRMWARE_GIGABOOT_CPP_EFI_VARIABLES_H_
