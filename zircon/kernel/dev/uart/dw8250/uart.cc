// Copyright 2019 The Fuchsia Authors
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/intrin.h>
#include <lib/cbuf.h>
#include <lib/debuglog.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zircon-internal/macros.h>
#include <lib/zx/result.h>
#include <reg.h>
#include <stdio.h>
#include <trace.h>

#include <dev/interrupt.h>
#include <dev/uart.h>
#include <dev/uart/dw8250/init.h>
#include <kernel/lockdep.h>
#include <kernel/thread.h>
#include <pdev/uart.h>
#include <platform/debug.h>

#if __aarch64__
#include <arch/arm64/periphmap.h>
#endif
#if __riscv
#include <vm/physmap.h>
#endif

// DW8250 implementation

// clang-format off
// UART Registers using 4 byte stride

#define UART_RBR                    (0x0)   // RX Buffer Register (read-only)
#define UART_THR                    (0x0)   // TX Buffer Register (write-only)
#define UART_DLL                    (0x0)   // Divisor Latch Low (Only when LCR[7] = 1)
#define UART_DLH                    (0x4)   // Divisor Latch High (Only when LCR[7] = 1)
#define UART_IER                    (0x4)   // Interrupt Enable Register
#define UART_IIR                    (0x8)   // Interrupt Identification Register (read-only)
#define UART_FCR                    (0x8)   // FIFO Control Register (write-only)
#define UART_LCR                    (0xc)   // Line Control Register
#define UART_MCR                    (0x10)  // Modem Control Register
#define UART_LSR                    (0x14)  // Line Status Register (read-only)
#define UART_MSR                    (0x18)  // Modem Status Register (read-only)
#define UART_SCR                    (0x1c)  // Scratch Register
#define UART_LPDLL                  (0x20)  // Low Power Divisor Latch (Low) Register
#define UART_LPDLH                  (0x24)  // Low Power Divisor Latch (High) Register
#define UART_SRBR                   (0x30)  // Shadow Receive Buffer Register (read-only)
#define UART_STHR                   (0x34)  // Shadow Transmit Holding Register
#define UART_FAR                    (0x70)  // FIFO Access Register
#define UART_TFR                    (0x74)  // Transmit FIFO Read Register (read-only)
#define UART_RFW                    (0x78)  // Receive FIFO Write Register (write-only)
#define UART_USR                    (0x7C)  // UART Status Register (read-only)
#define UART_TFL                    (0x80)  // Transmit FIFO Level Register (read-only)
#define UART_RFL                    (0x84)  // Receive FIFO Level Register (read-only)
#define UART_SRR                    (0x88)  // Software Reser Register
#define UART_SRTS                   (0x8C)  // Shadow Request to Send Register
#define UART_SBCR                   (0x90)  // Shadow Break Control Register
#define UART_SDMAM                  (0x94)  // Shadow DMA Mode Register
#define UART_SFE                    (0x98)  // Shadow FIFO Enable Register
#define UART_SRT                    (0x9C)  // Shadow RCVR Trigger Register
#define UART_STET                   (0xA0)  // Shadow TX Empty Trigger Register
#define UART_HTX                    (0xA4)  // Halt TX Register
#define UART_DMASA                  (0xA8)  // DMA Software Acknowledge Register (write-only)
#define UART_CPR                    (0xF4)  // Component Parameter Register (read-only)
#define UART_UCV                    (0xF8)  // UART Component Version Register (read-only)
#define UART_CTR                    (0xFC)  // Component Type Register

// IER
#define UART_IER_ERBFI              (1 << 0)
#define UART_IER_ETBEI              (1 << 1)
#define UART_IER_ELSI               (1 << 2)
#define UART_IER_EDSSI              (1 << 3)
#define UART_IER_PTIME              (1 << 5)

// IIR
#define UART_IIR_RLS                (0x06)  // Receiver Line Status
#define UART_IIR_RDA                (0x04)  // Receive Data Available
#define UART_IIR_BUSY               (0x07)  // Busy Detect Indication
#define UART_IIR_CTI                (0x0C)  // Character Timeout Indicator
#define UART_IIR_THRE               (0x02)  // Transmit Holding Register Empty
#define UART_IIR_MS                 (0x00)  // Check Modem Status Register
#define UART_IIR_SW_FLOW_CTRL       (0x10)  // Receive XOFF characters
#define UART_IIR_HW_FLOW_CTRL       (0x20)  // CTS or RTS Rising Edge
#define UART_IIR_FIFO_EN            (0xc0)
#define UART_IIR_INT_MASK           (0x1f)

// LSR
#define UART_LSR_DR                 (1 << 0)
#define UART_LSR_OE                 (1 << 1)
#define UART_LSR_PE                 (1 << 2)
#define UART_LSR_FE                 (1 << 3)
#define UART_LSR_BI                 (1 << 4)
#define UART_LSR_THRE               (1 << 5)
#define UART_LSR_TEMT               (1 << 6)
#define UART_LSR_FIFOERR            (1 << 7)
// clang-format on

#define RXBUF_SIZE 32

// values read from zbi
static bool initialized = false;
static vaddr_t uart_base = 0;
static uint32_t uart_irq = 0;

// are the registers 4 bytes wide on 4 byte intervals?
static bool stride4 = false;

static Cbuf uart_rx_buf;
static bool uart_tx_irq_enabled = false;
static AutounsignalEvent uart_dputc_event{true};

namespace {
// It's important to ensure that no other locks are acquired while holding this lock.  This lock is
// needed for the printf and panic code paths, and printing and panicking must be safe while holding
// (almost) any lock.
DECLARE_SINGLETON_SPINLOCK_WITH_TYPE(uart_spinlock, MonitoredSpinLock);
}  // namespace

static uint8_t uartreg_read(ptrdiff_t reg) {
  if (stride4) {
    return (*(volatile uint32_t*)((uart_base) + (reg))) & 0xff;
  } else {
    return (*(volatile uint8_t*)((uart_base) + (reg / 4U)));
  }
}

static void uartreg_write(ptrdiff_t reg, uint8_t val) {
  if (stride4) {
    (*(volatile uint32_t*)((uart_base) + (reg))) = val;
  } else {
    (*(volatile uint8_t*)((uart_base) + (reg / 4U))) = val;
  }
}

static void uartreg_and_eq(ptrdiff_t reg, uint8_t flags) {
  if (stride4) {
    volatile uint32_t* ptr = reinterpret_cast<volatile uint32_t*>(uart_base + reg);
    *ptr = *ptr & flags;
  } else {
    volatile uint8_t* ptr = reinterpret_cast<volatile uint8_t*>(uart_base + reg / 4U);
    *ptr = *ptr & flags;
  }
}

static void uartreg_or_eq(ptrdiff_t reg, uint8_t flags) {
  if (stride4) {
    volatile uint32_t* ptr = reinterpret_cast<volatile uint32_t*>(uart_base + reg);
    *ptr = *ptr | flags;
  } else {
    volatile uint8_t* ptr = reinterpret_cast<volatile uint8_t*>(uart_base + reg / 4U);
    *ptr = *ptr | flags;
  }
}

static void dw8250_uart_irq(void* arg) {
  if ((uartreg_read(UART_IIR) & UART_IIR_BUSY) == UART_IIR_BUSY) {
    // To clear the USR (UART Status Register) we need to read it.
    volatile auto unused = uartreg_read(UART_USR);
    static_cast<void>(unused);
  }

  // read interrupt status and mask
  while (uartreg_read(UART_LSR) & UART_LSR_DR) {
    if (uart_rx_buf.Full()) {
      break;
    }
    char c = uartreg_read(UART_RBR) & 0xFF;
    uart_rx_buf.WriteChar(c);
  }

  // Signal if anyone is waiting to TX
  if (uartreg_read(UART_LSR) & UART_LSR_THRE) {
    uartreg_and_eq(UART_IER, ~UART_IER_ETBEI);  // Disable TX interrupt
    // TODO(andresoportus): Revisit all UART drivers usage of events, from event.h:
    // 1. The reschedule flag is not supposed to be true in interrupt context.
    // 2. AutounsignalEvent only wakes up one thread per Signal().
    uart_dputc_event.Signal();
  }
}

/* panic-time getc/putc */
static void dw8250_uart_pputc(char c) {
  // spin while fifo is full
  while (!(uartreg_read(UART_LSR) & UART_LSR_THRE))
    ;
  uartreg_write(UART_THR, c);
}

static int dw8250_uart_pgetc() {
  // spin while fifo is empty
  while (!(uartreg_read(UART_LSR) & UART_LSR_DR))
    ;
  return uartreg_read(UART_RBR);
}

static int dw8250_uart_getc(bool wait) {
  if (initialized) {
    zx::result<char> result = uart_rx_buf.ReadChar(wait);
    if (result.is_ok()) {
      return result.value();
    }
    return result.error_value();
  } else {
    // Interrupts are not enabled yet. Use panic calls for now
    return dw8250_uart_pgetc();
  }
}

static void dw8250_dputs(const char* str, size_t len, bool block) {
  bool copied_CR = false;

  if (!uart_tx_irq_enabled) {
    block = false;
  }
  Guard<MonitoredSpinLock, IrqSave> guard{uart_spinlock::Get(), SOURCE_TAG};

  while (len > 0) {
    // is FIFO full?
    while (!(uartreg_read(UART_LSR) & UART_LSR_THRE)) {
      guard.CallUnlocked([&block]() {
        if (block) {
          uartreg_or_eq(UART_IER, UART_IER_ETBEI);  // Enable TX interrupt.
          uart_dputc_event.Wait();
        } else {
          arch::Yield();
        }
      });
    }
    if (*str == '\n' && !copied_CR) {
      copied_CR = true;
      dw8250_uart_pputc('\r');
    } else {
      copied_CR = false;
      dw8250_uart_pputc(*str++);
      len--;
    }
  }
}

void Dw8250UartInitLate() {
  // Initialize circular buffer to hold received data.
  uart_rx_buf.Initialize(RXBUF_SIZE, malloc(RXBUF_SIZE));

  if (dlog_bypass() == true) {
    uart_tx_irq_enabled = false;
    return;
  }

  zx_status_t status =
      configure_interrupt(uart_irq, IRQ_TRIGGER_MODE_LEVEL, IRQ_POLARITY_ACTIVE_HIGH);
  if (status != ZX_OK) {
    printf("UART: configure_interrupt failed %d\n", status);
    return;
  }

  status = register_permanent_int_handler(uart_irq, &dw8250_uart_irq, NULL);
  if (status != ZX_OK) {
    printf("UART: register_permanent_int_handler failed %d\n", status);
    return;
  }
  // enable interrupt
  status = unmask_interrupt(uart_irq);
  if (status != ZX_OK) {
    printf("UART: unmask_interrupt failed %d\n", status);
    return;
  }

  uartreg_or_eq(UART_IER, UART_IER_ERBFI);  // Enable RX interrupt.
  initialized = true;
  // Start up tx driven output.
  printf("UART: starting IRQ driven TX\n");
  uart_tx_irq_enabled = true;
}

static const struct pdev_uart_ops uart_ops = {
    .getc = dw8250_uart_getc,
    .pputc = dw8250_uart_pputc,
    .pgetc = dw8250_uart_pgetc,
    .dputs = dw8250_dputs,
};

extern "C" void uart_mark(unsigned char x);

void Dw8250UartInitEarly(const zbi_dcfg_simple_t& config, size_t stride) {
  ASSERT(config.mmio_phys != 0);
  ASSERT(config.irq != 0);

#if __aarch64__
  uart_base = periph_paddr_to_vaddr(config.mmio_phys);
#elif __riscv
  uart_base = reinterpret_cast<vaddr_t>(paddr_to_physmap(config.mmio_phys));
#else
#error define uart base logic for architecture
#endif
  ASSERT(uart_base != 0);
  uart_irq = config.irq;

  ASSERT(stride == 1 || stride == 4);
  stride4 = stride == 4;

  pdev_register_uart(&uart_ops);
}
