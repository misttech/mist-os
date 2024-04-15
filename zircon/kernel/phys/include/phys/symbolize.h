// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_SYMBOLIZE_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_SYMBOLIZE_H_

#include <lib/arch/backtrace.h>
#include <lib/elfldltl/note.h>
#include <lib/elfldltl/preallocated-vector.h>
#include <lib/symbolizer-markup/writer.h>
#include <stdint.h>
#include <stdio.h>

#include <ktl/algorithm.h>
#include <ktl/byte.h>
#include <ktl/declval.h>
#include <ktl/optional.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <phys/main.h>
#include <phys/stack.h>

#include "zircon/assert.h"

class ElfImage;
struct PhysExceptionState;
class Symbolize;

// The Symbolize instance registered by MainSymbolize.
extern Symbolize* gSymbolize;

class Symbolize {
 public:
  template <class BootStackType>
  struct Stack {
    BootStackType& boot_stack;
    std::string_view name;
  };

  struct IsOnStackFunction {
    bool operator()(const void* ptr) const {
      return gSymbolize && gSymbolize->IsOnStack(reinterpret_cast<uintptr_t>(ptr));
    }
  };
  using FramePointerBacktrace = arch::FramePointerBacktrace<IsOnStackFunction>;

  using ModuleList = elfldltl::PreallocatedVector<const ElfImage*>;

  Symbolize() = delete;
  Symbolize(const Symbolize&) = delete;

  explicit Symbolize(const char* name, FILE* f = stdout)
      : name_(name), output_(f), writer_(Sink{output_}) {}

  const char* name() const { return name_; }

  void set_name(const char* new_name) { name_ = new_name; }

  auto modules() const { return modules_.as_span(); }

  // Return the ELF build ID note for the currently executing module (i.e., the
  // first module to call OnLoad or the last module to call OnHandoff).
  elfldltl::ElfNote build_id() const;

  // This reads the existing modules to the new storage and then
  // replaces it as the storage to be used by future OnLoad calls.
  void ReplaceModulesStorage(ModuleList modules);

  void set_stacks(ktl::span<const Stack<BootStack>> stacks) { stacks_ = stacks; }

  void set_shadow_call_stacks(ktl::span<const Stack<BootShadowCallStack>> stacks) {
    shadow_call_stacks_ = stacks;
  }

  bool IsOnStack(uintptr_t sp) const;

  arch::ShadowCallStackBacktrace GetShadowCallStackBacktrace(
      uintptr_t scsp = arch::GetShadowCallStackPointer()) const;

  FramePointerBacktrace GetFramePointerBacktrace(
      const arch::CallFrame* fp = static_cast<arch::CallFrame*>(__builtin_frame_address(0))) const {
    return FramePointerBacktrace::BackTrace(fp);
  }

  // Print the contextual markup elements describing each loaded module.
  void ContextAlways(FILE* log = nullptr);

  // Same, but idempotent: the first call prints and others do nothing.
  void Context();

  // Adds the new module to the list.  If Context() has run, then emit context
  // for the new module.
  void OnLoad(const ElfImage& loaded);

  // Registers the next, just-handed-of-to module as the currently executing
  // one.
  void OnHandoff(ElfImage& next);

  void LogHandoff(ktl::string_view name, uintptr_t entry_pc);

  // Print the presentation markup element for one frame of a backtrace.
  void BackTraceFrame(unsigned int n, uintptr_t pc, bool interrupt = false);

  // Print a backtrace, ensuring context has been printed beforehand.
  // This takes any container of uintptr_t, so FramePointer works.
  template <typename T>
  PHYS_SINGLETHREAD void BackTrace(const T& pcs, unsigned int n, unsigned int max) {
    Context();
    for (uintptr_t pc : pcs) {
      if (max != 0 && n >= max) {
        writer_.Prefix(name_)
            .Literal("Backtrace truncated before frame #")
            .DecimalDigits(n)
            .Newline();
        break;
      }
      BackTraceFrame(n++, pc);
    }
  }

  // Print both flavors of backtrace together.  If the optional interrupt_pc
  // argument is supplied, then it's inserted as frame 0 and marked as exact PC
  // (whereas all frame-pointer and shadow-call-stack frames are marked as RA).
  PHYS_SINGLETHREAD void PrintBacktraces(const FramePointerBacktrace& frame_pointers,
                                         const arch::ShadowCallStackBacktrace& shadow_call_stack,
                                         ktl::optional<uintptr_t> interrupt_pc = ktl::nullopt);

  // Print the trigger markup element for a dumpfile.
  void DumpFile(ktl::string_view announce, size_t size_bytes, ktl::string_view sink_name,
                ktl::string_view vmo_name, ktl::string_view vmo_name_suffix = "");

  // Dump some stack up to the SP.
  PHYS_SINGLETHREAD void PrintStack(uintptr_t sp,
                                    ktl::optional<size_t> max_size_bytes = ktl::nullopt);

  // Print out register values.
  PHYS_SINGLETHREAD void PrintRegisters(const PhysExceptionState& regs);

  // Print out useful details at an exception.
  PHYS_SINGLETHREAD void PrintException(uint64_t vector, const char* vector_name,
                                        const PhysExceptionState& regs);

 protected:
  void set_main_module(const ElfImage& main) {
    ZX_DEBUG_ASSERT(!main_module_);
    main_module_ = &main;
  }

 private:
  struct Sink {
    FILE* f;

    int operator()(std::string_view str) const { return f->Write(str); }
  };

  void Printf(const char* fmt, ...);

  void AddModule(const ElfImage* module);

  const char* name_;
  FILE* output_;
  ModuleList modules_;
  ktl::span<const Stack<BootStack>> stacks_;
  ktl::span<const Stack<BootShadowCallStack>> shadow_call_stacks_;
  symbolizer_markup::Writer<Sink> writer_;
  // The currently executing module, set on the first call to LoadModule() or
  // the last to call OnHandoff().
  const ElfImage* main_module_ = nullptr;
  bool context_done_ = false;
};

// MainSymbolize represents the singleton Symbolize instance to be used by the
// current program. On construction, it regsters itself as `gSymbolize` and
// emits symbolization markup context.
class MainSymbolize : public Symbolize {
 public:
  explicit MainSymbolize(const char* name);

  const ElfImage& self() const { return *self_; }

  void set_self(const ElfImage* self);

 private:
  const ElfImage* self_ = nullptr;
};

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_SYMBOLIZE_H_
