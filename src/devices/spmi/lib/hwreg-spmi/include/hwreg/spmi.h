// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPMI_LIB_HWREG_SPMI_INCLUDE_HWREG_SPMI_H_
#define SRC_DEVICES_SPMI_LIB_HWREG_SPMI_INCLUDE_HWREG_SPMI_H_

#include <endian.h>
#include <fidl/fuchsia.hardware.spmi/cpp/wire.h>
#include <lib/zx/result.h>

#include <hwreg/bitfields.h>

namespace hwreg {

namespace internal {

inline zx::result<> MapError(fuchsia_hardware_spmi::DriverError error) {
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

// Methods to convert between host and big/little endian for the SPMI bus.
static inline uint32_t HostToBigEndian(uint32_t val) { return htobe32(val); }
static inline uint16_t HostToBigEndian(uint16_t val) { return htobe16(val); }
static inline uint8_t HostToBigEndian(uint8_t val) { return val; }
static inline uint32_t BigEndianToHost(uint32_t val) { return betoh32(val); }
static inline uint16_t BigEndianToHost(uint16_t val) { return betoh16(val); }
static inline uint8_t BigEndianToHost(uint8_t val) { return val; }
static inline uint32_t HostToLittleEndian(uint32_t val) { return htole32(val); }
static inline uint16_t HostToLittleEndian(uint16_t val) { return htole16(val); }
static inline uint8_t HostToLittleEndian(uint8_t val) { return val; }
static inline uint32_t LittleEndianToHost(uint32_t val) { return letoh32(val); }
static inline uint16_t LittleEndianToHost(uint16_t val) { return letoh16(val); }
static inline uint8_t LittleEndianToHost(uint8_t val) { return val; }

}  // namespace internal

struct LittleEndian;
struct BigEndian;

// Convert host to spmi byte order.
template <typename T, class ByteOrder>
static T ConvertToSpmiByteOrder(T value) {
  if (std::is_same<ByteOrder, BigEndian>::value) {
    // Big Endian
    return internal::HostToBigEndian(value);
  } else {
    // Little Endian / default (for 1 byte)
    return internal::HostToLittleEndian(value);
  }
}

// Convert from spmi byte order to host.
template <typename T, class ByteOrder>
static T ConvertFromSpmiByteOrder(T value) {
  if (std::is_same<ByteOrder, BigEndian>::value) {
    // Big Endian
    return internal::BigEndianToHost(value);
  } else {
    // Little Endian / default (for 1 byte)
    return internal::LittleEndianToHost(value);
  }
}

// An instance of SpmiRegisterBase represents a staging copy of a register,
// which can be written to the device's register using SPMI protocol. It knows the register's
// address and stores a value for the register. The actual write/read is done upon calling
// ReadFrom()/WriteTo() methods.
//
// All usage rules of RegisterBase applies here with the following exceptions
// - ReadFrom()/WriteTo() methods will return zx_status_t instead of the class object and hence
//   cannot be used in method chaining.
template <class DerivedType, class IntType, class ByteOrder = void, class PrinterState = void>
class SpmiRegisterBase : public RegisterBase<DerivedType, IntType, PrinterState> {
  static_assert(std::is_same<ByteOrder, void>::value ||
                    std::is_same<ByteOrder, LittleEndian>::value ||
                    std::is_same<ByteOrder, BigEndian>::value,
                "unsupported byte order");
  // Byte order must be specified if register address/value is more than one byte
  static_assert(!((sizeof(IntType) > 1) && std::is_same<ByteOrder, void>::value),
                "Byte order must be specified");

 public:
  using SpmiByteOrder = ByteOrder;
  // Delete base class ReadFrom() and WriteTo() methods and define new ones
  // which return zx_status_t in case of SPMI read/write failure
  template <typename T>
  DerivedType& ReadFrom(T* reg_io) = delete;
  template <typename T>
  DerivedType& WriteTo(T* mmio) = delete;

  using RegisterBaseType = RegisterBase<DerivedType, IntType, PrinterState>;

  zx::result<DerivedType> ReadFrom(const fidl::ClientEnd<fuchsia_hardware_spmi::Device>& client) {
    return ReadFrom(client.borrow());
  }

  zx::result<DerivedType> ReadFrom(
      const fidl::UnownedClientEnd<fuchsia_hardware_spmi::Device>& client) {
    uint32_t addr = RegisterBaseType::reg_addr();

    auto response = fidl::WireCall(client)->ExtendedRegisterReadLong(addr, sizeof(IntType));
    if (!response.ok()) {
      return zx::error(response.status());
    }
    if (response.value().is_error()) {
      return internal::MapError(response.value().error_value()).take_error();
    }

    if (response.value().value()->data.count() != sizeof(IntType)) {
      return zx::error(ZX_ERR_BAD_STATE);
    }

    IntType value;
    memcpy(&value, response.value().value()->data.data(), sizeof(value));
    value = ConvertFromSpmiByteOrder<IntType, SpmiByteOrder>(value);
    return zx::ok(RegisterBaseType::set_reg_value(value));
  }

  zx::result<> WriteTo(const fidl::ClientEnd<fuchsia_hardware_spmi::Device>& client) {
    return WriteTo(client.borrow());
  }

  zx::result<> WriteTo(const fidl::UnownedClientEnd<fuchsia_hardware_spmi::Device>& client) {
    uint32_t addr = RegisterBaseType::reg_addr();
    IntType value = RegisterBaseType::reg_value();

    value = ConvertToSpmiByteOrder<IntType, SpmiByteOrder>(value);

    std::vector<uint8_t> vector{static_cast<uint8_t*>(&value),
                                static_cast<uint8_t*>(&value) + sizeof(IntType)};
    auto response = fidl::WireCall(client)->ExtendedRegisterWriteLong(
        addr, fidl::VectorView<uint8_t>::FromExternal(vector));
    if (!response.ok()) {
      return zx::error(response.status());
    }
    if (response.value().is_error()) {
      return internal::MapError(response.value().error_value()).take_error();
    }
    return zx::ok();
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
      std::is_base_of<
          SpmiRegisterBase<RegType, typename RegType::ValueType, typename RegType::SpmiByteOrder>,
          RegType>::value ||
          std::is_base_of<SpmiRegisterBase<RegType, typename RegType::ValueType,
                                           typename RegType::SpmiByteOrder, EnablePrinter>,
                          RegType>::value,
      "Parameter of SpmiRegisterAddr<> should derive from SpmiRegisterAddr");

  // Delete base class ReadFrom() as read from SPMI can fail and
  // cannot be used for constructing RegType.
  template <typename T>
  RegType ReadFrom(T* reg_io) = delete;

  zx::result<RegType> ReadFrom(const fidl::ClientEnd<fuchsia_hardware_spmi::Device>& client) {
    return ReadFrom(client.borrow());
  }

  zx::result<RegType> ReadFrom(
      const fidl::UnownedClientEnd<fuchsia_hardware_spmi::Device>& client) {
    RegType reg;
    reg.set_reg_addr(RegisterAddr<RegType>::addr());
    return reg.ReadFrom(client);
  }

  SpmiRegisterAddr(uint32_t reg_addr) : RegisterAddr<RegType>(reg_addr) {}
};

// Helper for consolidating contiguous reads/writes to SPMI registers.
class SpmiRegisterArray {
 public:
  SpmiRegisterArray(uint32_t base_address, size_t size) : base_address_(base_address) {
    regs_.resize(size);
  }

  zx::result<SpmiRegisterArray> ReadFrom(
      const fidl::ClientEnd<fuchsia_hardware_spmi::Device>& client) {
    return ReadFrom(client.borrow());
  }

  zx::result<SpmiRegisterArray> ReadFrom(
      const fidl::UnownedClientEnd<fuchsia_hardware_spmi::Device>& client) {
    uint32_t address = base_address_;
    size_t size = regs_.size();
    auto data = regs_.data();

    while (size) {
      size_t read_size =
          std::min<size_t>(size, fuchsia_hardware_spmi::wire::kMaxExtendedLongTransferSize);
      auto response = fidl::WireCall(client)->ExtendedRegisterReadLong(address, read_size);
      if (!response.ok()) {
        return zx::error(response.status());
      }
      if (response.value().is_error()) {
        return internal::MapError(response.value().error_value()).take_error();
      }

      if (response.value().value()->data.count() != read_size) {
        return zx::error(ZX_ERR_BAD_STATE);
      }

      memcpy(data, response.value().value()->data.data(), read_size);

      size -= read_size;
      address += read_size;
      data += read_size;
    }
    return zx::ok(*this);
  }

  zx::result<> WriteTo(const fidl::ClientEnd<fuchsia_hardware_spmi::Device>& client) {
    return WriteTo(client.borrow());
  }

  zx::result<> WriteTo(const fidl::UnownedClientEnd<fuchsia_hardware_spmi::Device>& client) {
    uint32_t address = base_address_;
    size_t size = regs_.size();
    auto data = regs_.data();

    while (size) {
      size_t write_size =
          std::min<size_t>(size, fuchsia_hardware_spmi::wire::kMaxExtendedLongTransferSize);
      auto response = fidl::WireCall(client)->ExtendedRegisterWriteLong(
          address, fidl::VectorView<uint8_t>::FromExternal(data, write_size));
      if (!response.ok()) {
        return zx::error(response.status());
      }
      if (response.value().is_error()) {
        return internal::MapError(response.value().error_value()).take_error();
      }

      size -= write_size;
      address += write_size;
      data += write_size;
    }
    return zx::ok();
  }

  // `regs()` accesses register values of contiguous addresses from `base_address_` in
  // SpmiByteOrder. Callers of this function should use `CovertTo/FromSpmiByteOrder` to get the
  // correct endianness for host. Callers should also pay attention to alignment issues. If needed,
  // memcpy to/from a new buffer.
  std::vector<uint8_t>& regs() { return regs_; }

 private:
  uint32_t base_address_;
  std::vector<uint8_t> regs_;
};

}  // namespace hwreg

#endif  // SRC_DEVICES_SPMI_LIB_HWREG_SPMI_INCLUDE_HWREG_SPMI_H_
