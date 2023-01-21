// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_SYMBOLIZE_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_SYMBOLIZE_H_

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

class ElfImage;
class FramePointer;
class ShadowCallStackBacktrace;
struct PhysExceptionState;
class Symbolize;

// The Symbolize instance registered by MainSymbolize.
extern Symbolize* gSymbolize;

// Returns the name of the current program, according to the currently
// registered Symbolize object. If no Symbolize has yet been registered, then
// it is assumed that we were in an early set-up context before we have had a
// chance to construct one; in that case, "early-init" is returned.
const char* ProgramName();

class Symbolize {
 public:
  template <class BootStackType>
  struct Stack {
    BootStackType& boot_stack;
    std::string_view name;
  };

  using ModuleList = elfldltl::PreallocatedVector<const ElfImage*>;

  Symbolize() = delete;
  Symbolize(const Symbolize&) = delete;

  explicit Symbolize(const char* name, FILE* f = stdout)
      : name_(name), output_(f), writer_(Sink{output_}) {}

  const char* name() const { return name_; }

  auto modules() const { return modules_.as_span(); }

  // This readds the existing modules to the new storage and then
  // replaces it as the storage to be used by future OnLoad calls.
  void ReplaceModulesStorage(ModuleList modules);

  void set_stacks(ktl::span<const Stack<BootStack>> stacks) { stacks_ = stacks; }

  void set_shadow_call_stacks(ktl::span<const Stack<BootShadowCallStack>> stacks) {
    shadow_call_stacks_ = stacks;
  }

  bool IsOnStack(uintptr_t sp) const;

  ShadowCallStackBacktrace GetShadowCallStackBacktrace(
      uintptr_t scsp = GetShadowCallStackPointer()) const;

  // Return the ELF build ID note for the main executable.
  elfldltl::ElfNote BuildId() const;

  // Print the contextual markup elements describing each loaded module.
  void ContextAlways(FILE* log = nullptr);

  // Same, but idempotent: the first call prints and others do nothing.
  void Context();

  // Adds the new module to the list.  If Context() has run, then emit context
  // for the new module.
  void OnLoad(const ElfImage& loaded);

  void LogHandoff(ktl::string_view name, uintptr_t entry_pc);

  // Print the presentation markup element for one frame of a backtrace.
  void BackTraceFrame(unsigned int n, uintptr_t pc, bool interrupt = false);

  // Print a backtrace, ensuring context has been printed beforehand.
  // This takes any container of uintptr_t, so FramePointer works.
  template <typename T>
  PHYS_SINGLETHREAD void BackTrace(const T& pcs, unsigned int n = 0) {
    Context();
    for (uintptr_t pc : pcs) {
      BackTraceFrame(n++, pc);
    }
  }

  // Print both flavors of backtrace together.
  PHYS_SINGLETHREAD void PrintBacktraces(const FramePointer& frame_pointers,
                                         const ShadowCallStackBacktrace& shadow_call_stack,
                                         unsigned int n = 0);

  // Print the trigger markup element for a dumpfile.
  // TODO(mcgrathr): corresponds to a ZBI item
  void DumpFile(ktl::string_view type, ktl::string_view name, ktl::string_view desc,
                size_t size_bytes);

  // Dump some stack up to the SP.
  PHYS_SINGLETHREAD void PrintStack(uintptr_t sp,
                                    ktl::optional<size_t> max_size_bytes = ktl::nullopt);

  // Print out register values.
  PHYS_SINGLETHREAD void PrintRegisters(const PhysExceptionState& regs);

  // Print out useful details at an exception.
  PHYS_SINGLETHREAD void PrintException(uint64_t vector, const char* vector_name,
                                        const PhysExceptionState& regs);

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
