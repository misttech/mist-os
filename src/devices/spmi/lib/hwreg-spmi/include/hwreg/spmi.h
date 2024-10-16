// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPMI_LIB_HWREG_SPMI_INCLUDE_HWREG_SPMI_H_
#define SRC_DEVICES_SPMI_LIB_HWREG_SPMI_INCLUDE_HWREG_SPMI_H_

#include <fidl/fuchsia.hardware.spmi/cpp/wire.h>
#include <lib/zx/result.h>

#include <hwreg/bitfields.h>

namespace hwreg {

// An instance of SpmiRegisterBase represents a staging copy of a register,
// which can be written to the device's register using SPMI protocol. It knows the register's
// address and stores a value for the register. The actual write/read is done upon calling
// ReadFrom()/WriteTo() methods.
//
// All usage rules of RegisterBase applies here with the following exceptions
// - ReadFrom()/WriteTo() methods will return zx_status_t instead of the class object and hence
//   cannot be used in method chaining.
template <class DerivedType, class IntType, class PrinterState = void>
class SpmiRegisterBase : public RegisterBase<DerivedType, IntType, PrinterState> {
 public:
  // Delete base class ReadFrom() and WriteTo() methods and define new ones
  // which return zx_status_t in case of SPMI read/write failure
  template <typename T>
  DerivedType& ReadFrom(T* reg_io) = delete;
  template <typename T>
  DerivedType& WriteTo(T* mmio) = delete;

  using RegisterBaseType = RegisterBase<DerivedType, IntType, PrinterState>;

  zx::result<DerivedType> ReadFrom(const fidl::ClientEnd<fuchsia_hardware_spmi::Device>& client) {
    uint32_t addr = RegisterBaseType::reg_addr();

    auto response = fidl::WireCall(client)->ExtendedRegisterReadLong(addr, sizeof(IntType));
    if (!response.ok()) {
      return zx::error(response.status());
    }
    if (response.value().is_error()) {
      return MapError(response.value().error_value()).take_error();
    }

    if (response.value().value()->data.count() != sizeof(IntType)) {
      return zx::error(ZX_ERR_BAD_STATE);
    }

    IntType value;
    memcpy(&value, response.value().value()->data.data(), sizeof(value));
    return zx::ok(RegisterBaseType::set_reg_value(value));
  }

  zx::result<> WriteTo(const fidl::ClientEnd<fuchsia_hardware_spmi::Device>& client) {
    uint32_t addr = RegisterBaseType::reg_addr();
    IntType value = RegisterBaseType::reg_value();

    std::vector<IntType> vector{value};
    auto response = fidl::WireCall(client)->ExtendedRegisterWriteLong(
        addr, fidl::VectorView<IntType>::FromExternal(vector));
    if (!response.ok()) {
      return zx::error(response.status());
    }
    if (response.value().is_error()) {
      return MapError(response.value().error_value()).take_error();
    }
    return zx::ok();
  }

 private:
  zx::result<> MapError(fuchsia_hardware_spmi::DriverError error) {
    switch (error) {
      case fuchsia_hardware_spmi::DriverError::kInternal:
        return zx::error(ZX_ERR_INTERNAL);
      case fuchsia_hardware_spmi::DriverError::kNotSupported:
        return zx::error(ZX_ERR_NOT_SUPPORTED);
      case fuchsia_hardware_spmi::DriverError::kInvalidArgs:
        return zx::error(ZX_ERR_INVALID_ARGS);
      case fuchsia_hardware_spmi::DriverError::kBadState:
        return zx::error(ZX_ERR_BAD_STATE);
      case fuchsia_hardware_spmi::DriverError::kIoRefused:
        return zx::error(ZX_ERR_IO_REFUSED);
      default:
        return zx::error(ZX_ERR_NOT_FOUND);
    }
  }
};

// An instance of SpmiRegisterAddr represents a typed register address: It
// holds the address of the register in the SPMI device and
// the type of its contents, RegType.  RegType represents the register's
// bitfields.  RegType should be a subclass of SpmiRegisterBase.
//
// All usage rules of RegisterAddr applies here with the following exceptions
// - FromValue() is the only valid method for creating RegType.
// - reg_addr is stored in uint32_t and will be casted to the exact length in SpmiRegisterBase.
template <class RegType>
class SpmiRegisterAddr : public RegisterAddr<RegType> {
 public:
  static_assert(
      std::is_base_of<SpmiRegisterBase<RegType, typename RegType::ValueType>, RegType>::value ||
          std::is_base_of<SpmiRegisterBase<RegType, typename RegType::ValueType, EnablePrinter>,
                          RegType>::value,
      "Parameter of SpmiRegisterAddr<> should derive from SpmiRegisterAddr");

  // Delete base class ReadFrom() as read from SPMI can fail and
  // cannot be used for constructing RegType.
  template <typename T>
  RegType ReadFrom(T* reg_io) = delete;

  SpmiRegisterAddr(uint32_t reg_addr) : RegisterAddr<RegType>(reg_addr) {}
};
}  // namespace hwreg

#endif  // SRC_DEVICES_SPMI_LIB_HWREG_SPMI_INCLUDE_HWREG_SPMI_H_
