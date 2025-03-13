// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_UART_ALL_H_
#define LIB_UART_ALL_H_

#include <lib/fit/function.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <stdio.h>

#include <type_traits>
#include <utility>
#include <variant>

#include <hwreg/internal.h>

#include "amlogic.h"
#include "exynos-usi.h"
#include "geni.h"
#include "ns8250.h"
#include "null.h"
#include "pl011.h"
#include "uart.h"

namespace devicetree {
class PropertyDecoder;
}

namespace uart {

namespace internal {

struct DummyDriver : public null::Driver {
  template <typename... Args>
  static std::optional<DummyDriver> MaybeCreate(Args&&...) {
    return {};
  }

  using null::Driver::Driver;

  void Unparse(FILE*) const { ZX_PANIC("DummyDriver should never be called!"); }
};

// std::visit is not pure-PIC-friendly.  hwreg implements a limited version
// that is pure-PIC-friendly and that works for the cases here.  Reach in and
// steal that instead of copying the implementation here, since there isn't
// any particularly good "public" place to move it into instead.
using hwreg::internal::Visit;

}  // namespace internal

namespace all {

// uart::all::WithAllDrivers<Template, Args...> instantiates the template class
// Template<Args..., foo::Driver, bar::Driver, ...> for all the drivers
// supported by this kernel build (foo, bar, ...).  Using a template that takes
// a template template parameter is the only real way (short of macros) to have
// a single list of the supported uart::xyz::Driver implementations.
template <template <class... Drivers> class Template, typename... Args>
using WithAllDrivers = Template<
    // Any additional template arguments precede the arguments for each driver.
    Args...,
    // A default-constructed variant gets the null driver.
    null::Driver,
    // These drivers are potentially used on all machines.
    ns8250::Mmio32Driver, ns8250::Mmio8Driver, ns8250::Dw8250Driver, ns8250::PxaDriver,
#if defined(__aarch64__) || UART_ALL_DRIVERS
    amlogic::Driver, geni::Driver, pl011::Driver,
#endif
#if defined(__aarch64__) || defined(__riscv) || UART_ALL_DRIVERS
    exynos_usi::Driver,
#endif
#if defined(__x86_64__) || defined(__i386__) || UART_ALL_DRIVERS
    ns8250::PioDriver,
#endif
    // This is never used but permits a trailing comma above.
    internal::DummyDriver>;

// The hardware support object underlying whichever KernelDriver type is the
// active variant can be extracted into this type and then used to construct a
// new uart::all::KernelDriver instantiation in a different environment.
//
// The underlying UartDriver types and ktl::variant (aka std::variant) hold
// only non-pointer data that can be transferred directly from one environment
// to another, e.g. to hand off from physboot to the kernel.
using Driver = WithAllDrivers<std::variant>;

// This represents the a configuration tagged with a driver from the set of available drivers.
// Helper methods allows us to generate a `uart::all::Driver`.
//
// The provided configuration object has the following properties:
//    * `uart_type` alias for the uart driver type.
//    * `config_type` alias for `uart_type::config_type`.
//    * `operator()*` and `operator()->` return reference and pointer to the underlying
//    `config_type` object.
//
// Note: There is no driver `state` being held in this object, just the configuration.
template <typename UartDriver = Driver>
class Config {
 public:
  Config() = default;
  Config(const Config&) = default;
  Config(Config&&) = default;

  // Constructor to obtain configurations from a Uart.
  template <typename Uart>
  explicit(false) Config(const uart::Config<Uart>& config) : configs_(config) {}
  template <typename Uart>
  explicit Config(const Uart& uart) : configs_(uart::Config<Uart>(uart.config())) {}
  template <typename Uart, template <typename, IoRegisterType> class IoProvider, typename Sync>
  explicit Config(const uart::KernelDriver<Uart, IoProvider, Sync>& uart)
      : configs_(uart::Config<Uart>(uart.config())) {}

  Config& operator=(const Config&) = default;
  Config& operator=(Config&&) = default;

  template <typename Uart, template <typename, IoRegisterType> class IoProvider, typename Sync>
  Config& operator=(const uart::KernelDriver<Uart, IoProvider, Sync>& driver) {
    configs_ = uart::Config<Uart>(driver.config());
    return *this;
  }

  template <typename T>
  Config& operator=(const T& uart) {
    configs_ = uart::Config<T>(uart.config());
    return *this;
  }

  // Visitor to access the active configuration object.
  template <typename T>
  void Visit(T&& visitor) {
    internal::Visit(std::forward<T>(visitor), configs_);
  }

  template <typename T>
  void Visit(T&& visitor) const {
    internal::Visit(std::forward<T>(visitor), configs_);
  }

 private:
  template <typename T>
  struct Variant;

  template <typename... Args>
  struct Variant<std::variant<Args...>> {
    using type = std::variant<uart::Config<Args>...>;
  };

  typename Variant<UartDriver>::type configs_;
};

// Instantiates `uart::all::Driver` with `config`.
template <typename UartDriver = uart::all::Driver>
UartDriver MakeDriver(const uart::all::Config<UartDriver>& config) {
  UartDriver driver;
  config.Visit([&driver]<typename T>(const T& uart_config) {
    driver.template emplace<typename T::uart_type>(*uart_config);
  });
  return driver;
}

// uart::all::KernelDriver is a variant across all the KernelDriver types.
template <template <typename, IoRegisterType> class IoProvider, typename Sync,
          typename UartDriver = Driver>
class KernelDriver {
 public:
  using uart_type = UartDriver;

  // In default-constructed state, it's the null driver.
  KernelDriver() = default;

  // It can be copy-constructed from one of the supported uart::xyz::Driver
  // types to hand off the hardware state from a different instantiation.
  template <typename T>
  explicit KernelDriver(const T& uart) : variant_(uart) {}

  // ...or from another all::KernelDriver::uart() result.
  explicit KernelDriver(const uart_type& uart) { *this = uart; }

  explicit KernelDriver(const Config<UartDriver>& config) {
    config.Visit([this]<typename ConfigType>(const ConfigType& driver_config) {
      variant_.template emplace<OneDriver<typename ConfigType::uart_type>>(*driver_config);
    });
  }

  // Assignment is another way to reinitialize the configuration.
  KernelDriver& operator=(const uart_type& uart) {
    internal::Visit(
        [this](auto&& uart) {
          using ThisUart = std::decay_t<decltype(uart)>;
          variant_.template emplace<OneDriver<ThisUart>>(uart);
        },
        uart);
    return *this;
  }

  Config<UartDriver> config() const {
    Config<UartDriver> conf;
    Visit([&conf](const auto& driver) { conf = driver; });
    return conf;
  }

  // If this ZBI item matches a supported driver, instantiate that driver and
  // return true.  If nothing matches, leave the existing driver (default null)
  // in place and return false.  The expected procedure is to apply this to
  // each ZBI item in order, so that the latest one wins (e.g. one appended by
  // the boot loader will supersede one embedded in the original ZBI).
  bool Match(const zbi_header_t& header, const void* payload) { return DoMatch(header, payload); }

  // If |debug_port| (DBG2 Acpi Table) contains a configuration matching any existing driver,
  // instantiate that driver and return true.
  bool Match(const acpi_lite::AcpiDebugPortDescriptor& debug_port) { return DoMatch(debug_port); }

  // Returns an empty callable if no drivers match, otherwise returns a callable that will
  // instantiate the selected instance with the supplied configuration object.
  fit::inline_function<void(const zbi_dcfg_simple_t&)> MatchDevicetree(
      const devicetree::PropertyDecoder& decoder) {
    return DoMatch(decoder);
  }

  // This is like Match, but instead of matching a ZBI item, it matches a
  // string value for the "kernel.serial" boot option.
  bool Parse(std::string_view option) { return DoMatch(option); }

  // Write out a string that Parse() can read back to recreate the driver
  // state.  This doesn't preserve the driver state, only the configuration.
  void Unparse(FILE* out = stdout) const {
    Visit([out](const auto& active) { active.Unparse(out); });
  }

  // Apply f to selected driver.
  template <typename T, typename... Args>
  void Visit(T&& f, Args... args) {
    internal::Visit(VariantVisitor<T>{std::forward<T>(f)}, variant_, std::forward<Args>(args)...);
  }

  // Apply f to selected driver.
  template <typename T, typename... Args>
  void Visit(T&& f, Args... args) const {
    internal::Visit(VariantVisitor<T>{std::forward<T>(f)}, variant_, std::forward<Args>(args)...);
  }

  // Extract the hardware configuration and state.  The return type is const
  // just to make clear that this never returns a mutable reference like normal
  // accessors do, it always copies.
  const uart_type uart() const {
    uart_type driver;
    Visit([&driver](auto&& active) {
      const auto& uart = active.uart();
      driver.template emplace<std::decay_t<decltype(uart)>>(uart);
    });
    return driver;
  }

  // Takes ownership of the underlying hardware management and state. This object will be left
  // in an invalid state, and should be reinitialized before interacting with it.
  uart_type TakeUart() && {
    uart_type driver;
    Visit([&driver](auto&& active) { driver = std::move(active).TakeUart(); });
    // moved-from state.
    variant_.template emplace<std::monostate>();
    return driver;
  }

  // Returns true if the active `uart::KernelDriver<...>` is backed by `TargetUartDriver`.
  template <typename TargetUartDriver>
  bool holds_alternative() const {
    return std::holds_alternative<OneDriver<TargetUartDriver>>(variant_);
  }

 private:
  template <class Uart>
  using OneDriver = uart::KernelDriver<Uart, IoProvider, Sync>;
  template <class... Uart>
  using Variant = std::variant<OneDriver<Uart>..., std::monostate>;

  template <typename T>
  struct OneDriverVariant;

  template <typename... Args>
  struct OneDriverVariant<std::variant<Args...>> {
    using type = Variant<Args...>;
  };

  template <size_t I, typename... Args>
  bool TryOneMatch(Args&&... args) {
    using Try = std::variant_alternative_t<I, decltype(variant_)>;
    if constexpr (!std::is_same_v<Try, std::monostate>) {
      if (auto driver = Try::uart_type::MaybeCreate(std::forward<Args>(args)...)) {
        variant_.template emplace<I>(*driver);
        return true;
      }
    }
    return false;
  }

  template <size_t I>
  fit::inline_function<void(const zbi_dcfg_simple_t&)> TryOneMatch(
      const devicetree::PropertyDecoder& decoder) {
    using Try = std::variant_alternative_t<I, decltype(variant_)>;

    if constexpr (!std::is_same_v<Try, std::monostate>) {
      using ConfigType = typename Try::uart_type::config_type;
      if constexpr (std::is_same_v<ConfigType, zbi_dcfg_simple_t>) {
        if (Try::uart_type::MatchDevicetree(decoder)) {
          return [this](const zbi_dcfg_simple_t& config) { variant_.template emplace<I>(config); };
        }
      }
    }
    return nullptr;
  }

  template <size_t... I, typename... Args>
  auto DoMatchHelper(std::index_sequence<I...>, Args&&... args) {
    decltype(TryOneMatch<0>(std::forward<Args>(args)...)) result{};
    ((result = TryOneMatch<I>(std::forward<Args>(args)...)) || ...);
    return result;
  }

  template <typename... Args>
  auto DoMatch(Args&&... args) {
    constexpr auto n = std::variant_size_v<decltype(variant_)>;
    return DoMatchHelper(std::make_index_sequence<n>(), std::forward<Args>(args)...);
  }

  // The VariantVisitor can be called exactly once, so its call operator has
  // only the rvalue overload.  When called on the std::monostate, it always
  // crashes rather than invoking the delegate object.
  template <typename Delegate>
  struct VariantVisitor {
    template <typename T, typename... Args>
    void operator()(T&& alternative, Args&&... args) && {
      if constexpr (std::is_same_v<std::decay_t<T>, std::monostate>) {
        // We cannot use panic macros in this spot, since that would end up
        // calling printf, which may end up visiting the same uart, whose
        // invalid state (std::monostate) triggered this.  Even this will cause
        // a cascade of machine exceptions.
        __builtin_abort();
      } else {
        std::invoke(std::move(delegate), std::forward<T>(alternative), std::forward<Args>(args)...);
      }
    }

    Delegate delegate;
  };

  typename OneDriverVariant<UartDriver>::type variant_;
};

}  // namespace all
}  // namespace uart

#endif  // LIB_UART_ALL_H_
