// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_REMOTE_ABI_H_
#define LIB_LD_REMOTE_ABI_H_

#include "abi.h"
#include "remote-abi-heap.h"
#include "remote-abi-stub.h"
#include "remote-load-module.h"

namespace ld {

// The RemoteAbi object handles all the details of populating the passive ABI
// for a given dynamic linking domain in a remote process.  It takes the
// RemoteAbiStub collected from the stub dynamic linker ELF file and modifies
// the RemoteLoadModule representing the instance of the stub module, based on
// a list of RemoteLoadModule objects representing the load order of modules
// going into the remote dynamic linking domain.
//
// There are only two methods: Init is called after decoding all modules but
// before laying out the remote address space; Finish is called after address
// space layout is known to finalize the data.

template <class Elf = elfldltl::Elf<>>
class RemoteAbi {
 public:
  using size_type = typename Elf::size_type;
  using Addr = typename Elf::Addr;

  using AbiStub = RemoteAbiStub<Elf>;
  using RemoteModule = RemoteLoadModule<Elf>;
  using ModuleList = typename RemoteModule::List;

  // Lay out the final stub data segment given the full decoded module list.
  // Then modify the decoded stub_module (which could be a copy of the one that
  // was used to initialize abi_stub, or the same one) so its mutable segment
  // is replaced by a longer ConstantSegment.  After this, the stub_module has
  // its final vaddr_size; load addresses can be selected for all modules.
  template <class Diagnostics>
  zx::result<> Init(Diagnostics& diag, const AbiStub& abi_stub, RemoteModule& stub_module,
                    const ModuleList& modules) {
    RemoteAbiHeapLayout layout{abi_stub.data_size()};

    // TODO(https://fxbug.dev/318041873): lay out heap arrays & strings here

    auto result = AbiHeap::Create(diag, abi_stub.data_size(), stub_module, std::move(layout));
    if (result.is_error()) {
      return result.take_error();
    }

    heap_.emplace(*std::move(result));
    return zx::ok();
  }

  // After every module's load_bias() is finalized, this writes the data out.
  // This must be the last method called.  After this, the stub_module is ready
  // to be loaded into the target address space along with the other modules.
  zx::result<> Finish(const AbiStub& abi_stub, const RemoteModule& stub_module,
                      const ModuleList& modules) && {
    Abi& abi = heap_->template Local<Abi>(abi_stub.abi_offset());
    FillAbi(abi, modules);

    RDebug& r_debug = heap_->template Local<RDebug>(abi_stub.rdebug_offset());
    FillRDebug(r_debug, abi, stub_module.load_bias());

    // Write the data into the stub data segment's VMO.
    return std::move(*heap_).Commit();
  }

 private:
  struct PtrTraits : public elfldltl::RemoteAbiTraits {};

  using AbiHeap = RemoteAbiHeap<Elf, PtrTraits>;

  using Abi = abi::Abi<Elf, PtrTraits>;
  using RDebug = typename Elf::template RDebug<PtrTraits>;
  using LinkMap = typename decltype(std::declval<RDebug>().map)::value_type;

  void FillAbi(Abi& abi, const ModuleList& modules) {
    // TODO(https://fxbug.dev/318041873): fill remote _ld_abi here
    abi = {};
  }

  void FillRDebug(RDebug& r_debug, const Abi& abi, size_type stub_load_bias) {
    // There's isn't much to _r_debug, so just fill it out directly.
    r_debug.version = elfldltl::kRDebugVersion;
    r_debug.map = abi.loaded_modules.template Reinterpret<LinkMap>();
    r_debug.state = elfldltl::RDebugState::kConsistent;
    r_debug.ldbase = stub_load_bias;
  }

  std::optional<AbiHeap> heap_;
};

}  // namespace ld

#endif  // LIB_LD_REMOTE_ABI_H_
