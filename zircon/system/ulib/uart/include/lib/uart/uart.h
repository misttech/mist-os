// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_UART_UART_H_
#define LIB_UART_UART_H_

#include <lib/arch/intrin.h>
#include <lib/devicetree/devicetree.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zircon-internal/macros.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <zircon/assert.h>

#include <cstdlib>
#include <optional>
#include <string_view>
#include <type_traits>
#include <utility>

#include <hwreg/mmio.h>
#include <hwreg/pio.h>

#include "chars-from.h"
#include "sync.h"

namespace acpi_lite {
struct AcpiDebugPortDescriptor;
}

namespace uart {

//
// These types are used in configuring the line control settings (i.e., in the
// `SetLineControl()` method).
//

// Number of bits transmitted per character.
enum class DataBits {
  k5,
  k6,
  k7,
  k8,
};

// The bit pattern mechanism to help detect transmission errors.
enum class Parity {
  kNone,  // No bits dedicated to parity.
  kEven,  // Parity bit present; is 0 iff the number of 1s in the word is even.
  kOdd,   // Parity bit present; is 0 iff the number of 1s in the word is odd.
};

// The duration of the stop period in terms of the transmitted bit rate.
enum class StopBits {
  k1,
  k2,
};

// This template is specialized by payload configuration type (see below).
// It parses bits out of strings from the "kernel.serial" boot option.
template <typename Config>
inline std::optional<Config> ParseConfig(std::string_view) {
  static_assert(std::is_void_v<Config>, "missing specialization");
  return {};
}

// This template is specialized by payload configuration type (see below).
// It recreates a string for Parse.
template <typename Config>
inline void UnparseConfig(const Config& config, FILE* out) {
  static_assert(std::is_void_v<Config>, "missing specialization");
}

// Specific hardware support is implemented in a class uart::xyz::Driver,
// referred to here as UartDriver.  The uart::DriverBase template class
// provides a helper base class for UartDriver implementations.
//
// The UartDriver object represents the hardware itself.  Many UartDriver
// classes hold no state other than the initial configuration data used in the
// constructor, but a UartDriver is not required to be stateless.  However, a
// UartDriver is required to be copy-constructible, trivially destructible,
// and contain no pointers.  This makes it safe to copy an object set up by
// physboot into a new object in the virtual-memory kernel to hand off the
// configuration and the state of the hardware.
//
// All access to the UartDriver object is serialized by its caller, so it does
// no synchronization of its own.  This serves to serialize the actual access
// to the hardware.
//
// The UartDriver API fills four roles:
//  1. Match a ZBI item that configures this driver.
//  2. Generate a ZBI item for another kernel to match this configuration.
//  3. Configure the IoProvider (see below).
//  4. Drive the actual hardware.
//
// The first three are handled by DriverBase.  The KdrvExtra and KdrvConfig
// template arguments give the ZBI_KERNEL_DRIVER_* value and the zbi_dcfg_*_t type for the ZBI
// item.  The Pio template argument tells the IoProvider whether this driver
// uses MMIO or PIO (including PIO via MMIO): the number of consecutive PIO
// ports used, or 0 for simple MMIO.
//
// Item matching is done by the MaybeCreate static method.  If the item
// matches KdrvExtra, then the UartDriver(KdrvConfig) constructor is called.
// DriverBase provides this constructor to fill the cfg_ field, which the
// UartDriver can then refer to.  The UartDriver copy constructor copies it.
//
// The calls to drive the hardware are all template functions passed an
// IoProvider object (see below).  The driver accesses device registers using
// hwreg ReadFrom and WriteTo calls on the pointers from the provider.  The
// IoProvider constructed is passed uart.config() and uart.pio_size().
//
template <typename Driver, uint32_t KdrvExtra, typename KdrvConfig, uint16_t Pio = 0>
class DriverBase {
 public:
  using config_type = KdrvConfig;

  // No devicetree bindings by default.
  static constexpr std::array<std::string_view, 0> kDevicetreeBindings{};

  explicit DriverBase(const config_type& cfg) : cfg_(cfg) {}

  constexpr bool operator==(const Driver& other) const {
    return memcmp(&cfg_, &other.cfg_, sizeof(cfg_)) == 0;
  }
  constexpr bool operator!=(const Driver& other) const { return !(*this == other); }

  // API to fill a ZBI item describing this UART.
  constexpr uint32_t type() const { return ZBI_TYPE_KERNEL_DRIVER; }
  constexpr uint32_t extra() const { return KdrvExtra; }
  constexpr size_t size() const { return sizeof(cfg_); }
  void FillItem(void* payload) const { memcpy(payload, &cfg_, sizeof(cfg_)); }

  // API to match a ZBI item describing this UART.
  static std::optional<Driver> MaybeCreate(const zbi_header_t& header, const void* payload) {
    static_assert(alignof(config_type) <= ZBI_ALIGNMENT);
    if (header.type == ZBI_TYPE_KERNEL_DRIVER && header.extra == KdrvExtra &&
        header.length >= sizeof(config_type)) {
      return Driver{*reinterpret_cast<const config_type*>(payload)};
    }
    return {};
  }

  // API to match a configuration string.
  static std::optional<Driver> MaybeCreate(std::string_view string) {
    const auto config_name = Driver::config_name();
    if (string.substr(0, config_name.size()) == config_name) {
      string.remove_prefix(config_name.size());
      auto config = ParseConfig<KdrvConfig>(string);
      if (config) {
        return Driver{*config};
      }
    }
    return {};
  }

  // API to match a devicetree bindings.
  static bool MatchDevicetree(const devicetree::PropertyDecoder& decoder) {
    if constexpr (Driver::kDevicetreeBindings.size() == 0) {
      return false;
    } else {
      auto compatible = decoder.FindProperty("compatible");
      if (!compatible) {
        return false;
      }

      auto compatible_list = compatible->AsStringList();
      if (!compatible_list) {
        return false;
      }

      return std::find_first_of(compatible_list->begin(), compatible_list->end(),
                                Driver::kDevicetreeBindings.begin(),
                                Driver::kDevicetreeBindings.end()) != compatible_list->end();
    }
  }

  // API to match DBG2 Table (ACPI). Currently only 16550 compatible uarts are supported.
  static std::optional<Driver> MaybeCreate(const acpi_lite::AcpiDebugPortDescriptor& debug_port) {
    return {};
  }

  // API to reproduce a configuration string.
  void Unparse(FILE* out) const {
    fprintf(out, "%.*s", static_cast<int>(Driver::config_name().size()),
            Driver::config_name().data());
    UnparseConfig<KdrvConfig>(cfg_, out);
  }

  // TODO(fxbug.dev/102726): Remove once all drivers define this method.
  template <class IoProvider>
  void SetLineControl(IoProvider& io, std::optional<DataBits> data_bits,
                      std::optional<Parity> parity, std::optional<StopBits> stop_bits) {
    static_assert(!std::is_same_v<IoProvider, IoProvider>, "TODO(fxbug.dev/102726): implment me");
  }

  // API for use in IoProvider setup.
  const config_type& config() const { return cfg_; }
  constexpr uint16_t pio_size() const { return Pio; }

 protected:
  config_type cfg_;

 private:
  template <typename T>
  static constexpr bool Uninstantiated = false;

  // uart::KernelDriver API
  //
  // These are here just to cause errors if Driver is missing its methods.
  // They also serve to document the API required by uart::KernelDriver.
  // They should all be overridden by Driver methods.
  //
  // Each method is a template parameterized by an hwreg-compatible type for
  // accessing the hardware registers via hwreg ReadFrom and WriteTo methods.
  // This lets Driver types be used with hwreg::Mock in tests independent of
  // actual hardware access.

  template <typename IoProvider>
  void Init(IoProvider& io) {
    static_assert(Uninstantiated<IoProvider>, "derived class is missing Init");
  }

  // Return true if Write can make forward progress right now.
  // The return value can be anything contextually convertible to bool.
  // The value will be passed on to Write.
  template <typename IoProvider>
  bool TxReady(IoProvider& io) {
    static_assert(Uninstantiated<IoProvider>, "derived class is missing TxReady");
    return false;
  }

  // This is called only when TxReady() has just returned something that
  // converts to true; that's passed here so it can convey more information
  // such as a count.  Advance the iterator at least one and as many as is
  // convenient but not past end, outputting each character before advancing.
  template <typename IoProvider, typename It1, typename It2>
  auto Write(IoProvider& io, bool ready, It1 it, const It2& end) {
    static_assert(Uninstantiated<IoProvider>, "derived class is missing Write");
    return end;
  }

  // Poll for an incoming character and return one if there is one.
  template <typename IoProvider>
  std::optional<uint8_t> Read(IoProvider& io) {
    static_assert(Uninstantiated<IoProvider>, "derived class is missing Read");
    return {};
  }

  // Set the UART up to deliver interrupts.  This is called after Init.
  // After this, Interrupt (below) may be called from interrupt context.
  template <typename IoProvider>
  void InitInterrupt(IoProvider& io) {
    static_assert(Uninstantiated<IoProvider>, "derived class is missing InitInterrupt");
  }

  // Enable transmit interrupts so Interrupt will be called when TxReady().
  template <typename IoProvider>
  void EnableTxInterrupt(IoProvider& io) {
    static_assert(Uninstantiated<IoProvider>, "derived class is missing EnableTxInterrupt");
  }

  // Service an interrupt.
  // Call tx(sync, disable_tx_irq) if transmission has become ready.
  // If receiving has become ready, call rx(sync, read_char, full) one or more
  // times, where read_char() -> uint8_t if there is receive buffer
  // space and full() -> void if there is no space.
  // |sync| provides access to the environment specific synchronization primitives(if any),
  // and synchronization related data structures(if any).
  template <typename IoProvider, typename Sync, typename Tx, typename Rx>
  void Interrupt(IoProvider& io, Sync& sync, Tx&& tx, Rx&& rx) {
    static_assert(Uninstantiated<IoProvider>, "derived class is missing Interrupt");
  }
};

// The IoProvider is a template class parameterized by UartDriver::config_type,
// i.e. the zbi_dcfg_*_t type for the ZBI item's format.  This class is responsible
// for supplying pointers to be passed to hwreg types' ReadFrom and WriteTo.
//
// The IoProvider(UartDriver::config_type, uint16_t pio_size) constructor
// initializes the object.  Then IoProvider::io() is called to yield the
// pointer to pass to hwreg calls.
//
// uart::BasicIoProvider handles the simple case where physical MMIO and PIO
// base addresses are used directly.  It also provides base classes that can be
// subclassed with an overridden constructor that does address mapping.
//
template <typename Config>
class BasicIoProvider;

// This is the default "identity mapping" callback for BasicIoProvider::Init.
// A subclass can have an Init function that calls BasicIoProvider::Init with
// a different callback function.
inline auto DirectMapMmio(uint64_t phys) { return reinterpret_cast<volatile void*>(phys); }

// The specialization used most commonly handles simple MMIO devices.
template <>
class BasicIoProvider<zbi_dcfg_simple_t> {
 public:
  // Just install the MMIO base pointer.  The third argument can be passed by
  // a subclass constructor method to map the physical address to a virtual
  // address.
  template <typename T>
  BasicIoProvider(const zbi_dcfg_simple_t& cfg, uint16_t pio_size, T&& map_mmio)
      : pio_size_(pio_size) {
    auto ptr = map_mmio(cfg.mmio_phys);
    if (pio_size != 0) {
      // Scaled MMIO with 32-byte I/O access.
      io_.emplace<hwreg::RegisterMmioScaled<uint32_t>>(ptr);
    } else {
      // This is normal MMIO.
      io_.emplace<hwreg::RegisterMmio>(ptr);
    }
  }

  BasicIoProvider(const zbi_dcfg_simple_t& cfg, uint16_t pio_size)
      : BasicIoProvider(cfg, pio_size, DirectMapMmio) {}

  BasicIoProvider& operator=(BasicIoProvider&& other) {
    io_.swap(other.io_);
    pio_size_ = other.pio_size_;
    return *this;
  }

  auto* io() { return &io_; }

  uint16_t pio_size() const { return pio_size_; }

 private:
  std::variant<hwreg::RegisterMmio, hwreg::RegisterMmioScaled<uint32_t>> io_{std::in_place_index<0>,
                                                                             nullptr};
  uint16_t pio_size_;
};

// The specialization for devices using actual PIO only occurs on x86.
#if defined(__x86_64__) || defined(__i386__)
template <>
class BasicIoProvider<zbi_dcfg_simple_pio_t> {
 public:
  BasicIoProvider(const zbi_dcfg_simple_pio_t& cfg, uint16_t pio_size) : io_(cfg.base) {
    ZX_DEBUG_ASSERT(pio_size > 0);
  }

  auto* io() { return &io_; }

 private:
  hwreg::RegisterDirectPio io_;
};
#endif

// Forward declaration.
namespace mock {
class Driver;
}

// The KernelDriver template class is parameterized by those three to implement
// actual driver logic for some environment.
//
// The KernelDriver constructor just passes its arguments through to the
// UartDriver constructor.  So it can be created directly from a configuration
// struct or copied from another UartDriver object.  In this way, the device is
// handed off from one KernelDriver instantiation to a different one using a
// different IoProvider (physboot vs kernel) and/or Sync (polling vs blocking).
//
template <class UartDriver, template <typename> class IoProvider, class SyncPolicy>
class KernelDriver {
  using Waiter = typename SyncPolicy::Waiter;

  template <typename LockPolicy>
  using Guard = typename SyncPolicy::template Guard<LockPolicy>;

  template <typename MemberOf>
  using Lock = typename SyncPolicy::template Lock<MemberOf>;

  using DefaultLockPolicy = typename SyncPolicy::DefaultLockPolicy;

 public:
  using uart_type = UartDriver;
  static_assert(std::is_copy_constructible_v<uart_type>);
  static_assert(std::is_trivially_destructible_v<uart_type> ||
                std::is_same_v<uart_type, mock::Driver>);

  // This sets up the object but not the device itself.  The device might
  // already have been set up by a previous instantiation's Init function,
  // or might never actually be set up because this instantiation gets
  // replaced with a different one before ever calling Init.
  template <typename... Args>
  explicit KernelDriver(Args&&... args)
      : uart_(std::forward<Args>(args)...), io_(uart_.config(), uart_.pio_size()) {
    if constexpr (std::is_same_v<mock::Driver, uart_type>) {
      // Initialize the mock sync object with the mock driver.
      lock_.Init(uart_);
      waiter_.Init(uart_);
    }
  }

  // Access underlying hardware driver object.
  const auto& uart() const { return uart_; }
  auto& uart() { return uart_; }

  // Access IoProvider object.
  auto& io() { return io_; }

  // Set up the device for nonblocking output and polling input.
  // If the device is handed off from a different instantiation,
  // this won't be called in the new instantiation.
  template <typename LockPolicy = DefaultLockPolicy>
  void Init() {
    Guard<LockPolicy> lock(&lock_, SOURCE_TAG);
    uart_.Init(io_);
  }

  // Write out a string that Parse() can read back to recreate the driver
  // state.  This doesn't preserve the driver state, only the configuration.
  template <typename LockPolicy = DefaultLockPolicy>
  void Unparse(FILE* out) const {
    Guard<LockPolicy> lock(&lock_, SOURCE_TAG);
    uart_.Unparse(out);
  }

  // Configure the UART line control settings.
  //
  // An individual setting given by std::nullopt signifies that it should be
  // left as previously configured.
  //
  // TODO(fxbug.dev/102726): Separate out the setting of baud rate.
  template <typename LockPolicy = DefaultLockPolicy>
  void SetLineControl(std::optional<DataBits> data_bits = DataBits::k8,
                      std::optional<Parity> parity = Parity::kNone,
                      std::optional<StopBits> stop_bits = StopBits::k1) {
    Guard<LockPolicy> lock(&lock_, SOURCE_TAG);
    uart_.SetLineControl(io_, data_bits, parity, stop_bits);
  }

  // TODO(fxbug.dev/129378): Asses the need of |enable_interrupt_callback|.
  template <typename LockPolicy = DefaultLockPolicy, typename EnableInterruptCallback>
  void InitInterrupt(EnableInterruptCallback&& enable_interrupt_callback) {
    Guard<LockPolicy> lock(&lock_, SOURCE_TAG);
    uart_.InitInterrupt(io_, std::forward<EnableInterruptCallback>(enable_interrupt_callback));
  }

  template <typename Tx, typename Rx>
  void Interrupt(Tx&& tx, Rx&& rx) TA_NO_THREAD_SAFETY_ANALYSIS {
    // Interrupt is responsible for properly acquiring and releasing sync
    // where needed.
    uart_.Interrupt(io_, lock_, waiter_, std::forward<Tx>(tx), std::forward<Rx>(rx));
  }

  // This is the FILE-compatible API: `FILE::stdout_ = FILE{&driver};`.
  template <typename LockPolicy = DefaultLockPolicy>
  int Write(std::string_view str) {
    uart::CharsFrom chars(str);  // Massage into uint8_t with \n -> CRLF.
    auto it = chars.begin();
    Guard<LockPolicy> lock(&lock_, SOURCE_TAG);
    while (it != chars.end()) {
      // Wait until the UART is ready for Write.
      auto ready = uart_.TxReady(io_);
      while (!ready) {
        // Block or just unlock and spin or whatever "wait" means to Sync.
        // If that means blocking for interrupt wakeup, enable tx interrupts.
        waiter_.Wait(lock, [this]() {
          SyncPolicy::AssertHeld(lock_);
          uart_.EnableTxInterrupt(io_);
        });
        ready = uart_.TxReady(io_);
      }
      // Advance the iterator by writing some.
      it = uart_.Write(io_, std::move(ready), it, chars.end());
    }
    return static_cast<int>(str.size());
  }

  // This is a direct polling read, not used in interrupt-based operation.
  template <typename LockPolicy = DefaultLockPolicy>
  auto Read() {
    Guard<LockPolicy> lock(&lock_, SOURCE_TAG);
    return uart_.Read(io_);
  }

  template <typename LockPolicy = DefaultLockPolicy>
  void EnableRxInterrupt() {
    Guard<LockPolicy> lock(&lock_, SOURCE_TAG);
    uart_.EnableRxInterrupt(io_);
  }

 private:
  Lock<KernelDriver> lock_;
  Waiter waiter_ TA_GUARDED(lock_);
  uart_type uart_ TA_GUARDED(lock_);

  IoProvider<typename uart_type::config_type> io_;
};

// These specializations are defined in the library to cover all the ZBI item
// payload types used by the various drivers.

template <>
std::optional<zbi_dcfg_simple_t> ParseConfig<zbi_dcfg_simple_t>(std::string_view string);

template <>
void UnparseConfig(const zbi_dcfg_simple_t& config, FILE* out);

template <>
std::optional<zbi_dcfg_simple_pio_t> ParseConfig<zbi_dcfg_simple_pio_t>(std::string_view string);

template <>
void UnparseConfig(const zbi_dcfg_simple_pio_t& config, FILE* out);

}  // namespace uart

#endif  // LIB_UART_UART_H_
