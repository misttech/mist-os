// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_SYMBOLS_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_SYMBOLS_H_

#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/vector.h>

namespace mistos {

struct device_t {
  const char* name;
  void* context;
};

constexpr device_t kDefaultDevice = {
    .name = "mistos-device",
    .context = nullptr,
};

// The symbol for the compat device: device_t.
constexpr char kDeviceSymbol[] = "mistos.device/Device";

class Symbol {
 public:
  Symbol(std::optional<std::string_view> name, std::optional<zx_vaddr_t> address)
      : name_(name), address_(address) {}

  const std::optional<std::string_view>& name() const { return name_; }
  const std::optional<zx_vaddr_t>& address() const { return address_; }

 private:
  std::optional<std::string_view> name_;
  std::optional<zx_vaddr_t> address_;
};

class DriverStartArgs {
 public:
  DriverStartArgs() = default;
  DriverStartArgs(fbl::Vector<Symbol> symbols) : symbols_(std::move(symbols)) {}
  DriverStartArgs(zx_vaddr_t driver_base, fbl::Vector<Symbol> symbols)
      : driver_base_(driver_base), symbols_(std::move(symbols)) {}

  void set_driver_base(zx_vaddr_t base) { driver_base_ = base; }
  zx_vaddr_t driver_base() const { return driver_base_; }
  const fbl::Vector<Symbol>& symbols() const { return symbols_; }

  bool has_symbols() const { return !symbols_.is_empty(); }

 private:
  zx_vaddr_t driver_base_ = 0;
  fbl::Vector<Symbol> symbols_;
};

#if 0
template <typename T>
zx::result<T> SymbolValue(const DriverStartArgs& args, std::string_view name) {
  if (!args.has_symbols()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  const fbl::Vector<Symbol>& symbols = args.symbols();
  static_assert(sizeof(T) == sizeof(zx_vaddr_t), "T must match zx_vaddr_t in size");
  for (auto& symbol : symbols) {
    if (std::equal(name.begin(), name.end(), symbol.name().begin())) {
      T value;
      memcpy(&value, &symbol.address(), sizeof(zx_vaddr_t));
      return zx::ok(value);
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}
#endif

template <typename T>
zx::result<T> SymbolValue(const fbl::Vector<Symbol>& symbols, std::string_view name) {
  if (!symbols.is_empty()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  static_assert(sizeof(T) == sizeof(zx_vaddr_t), "T must match zx_vaddr_t in size");
  for (auto& symbol : symbols) {
    if (!symbol.name().has_value() || name != symbol.name().value()) {
      continue;
    }
    if (!symbol.address().has_value()) {
      continue;
    }
    T value;
    memcpy(&value, &symbol.address().value(), sizeof(zx_vaddr_t));
    return zx::ok(value);
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

template <typename T>
T GetSymbol(const fbl::Vector<Symbol>& symbols, std::string_view name, T default_value = nullptr) {
  auto value = SymbolValue<T>(symbols, name);
  return value.is_ok() ? *value : default_value;
}

}  // namespace mistos

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_SYMBOLS_H_
