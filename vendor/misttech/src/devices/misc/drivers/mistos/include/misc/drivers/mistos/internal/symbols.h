// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_INTERNAL_SYMBOLS_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_INTERNAL_SYMBOLS_H_

template <typename T>
T GetSymbol(const std::optional<std::vector<fuchsia_driver_framework::NodeSymbol>>& symbols,
            std::string_view name, T default_value = nullptr) {
  auto value = SymbolValue<T>(symbols, name);
  return value.is_ok() ? *value : default_value;
}

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_INTERNAL_SYMBOLS_H_
