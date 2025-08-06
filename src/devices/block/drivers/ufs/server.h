// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_SERVER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_SERVER_H_

#include <fidl/fuchsia.hardware.ufs/cpp/fidl.h>
#include <fidl/fuchsia.hardware.ufs/cpp/wire_types.h>

#include <vector>

#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"

namespace ufs {

using fuchsia_hardware_ufs::wire::QueryErrorCode;

class UfsServer : public fidl::WireServer<fuchsia_hardware_ufs::Ufs> {
 public:
  explicit UfsServer(Ufs *ufs) : controller_(ufs) {}

  // fidl::WireServer<fuchsia_hardware_ufs::Ufs>
  void ReadDescriptor(ReadDescriptorRequestView request,
                      ReadDescriptorCompleter::Sync &completer) override;
  void WriteDescriptor(WriteDescriptorRequestView request,
                       WriteDescriptorCompleter::Sync &completer) override;
  void ReadFlag(ReadFlagRequestView request, ReadFlagCompleter::Sync &completer) override;
  void SetFlag(SetFlagRequestView request, SetFlagCompleter::Sync &completer) override;
  void ClearFlag(ClearFlagRequestView request, ClearFlagCompleter::Sync &completer) override;
  void ToggleFlag(ToggleFlagRequestView request, ToggleFlagCompleter::Sync &completer) override;
  void ReadAttribute(ReadAttributeRequestView request,
                     ReadAttributeCompleter::Sync &completer) override;
  void WriteAttribute(WriteAttributeRequestView request,
                      WriteAttributeCompleter::Sync &completer) override;
  void SendUicCommand(SendUicCommandRequestView request,
                      SendUicCommandCompleter::Sync &completer) override;
  void Request(RequestRequestView request, RequestCompleter::Sync &completer) override;

 private:
  template <typename ResponseUpiu>
  fit::result<fuchsia_hardware_ufs::wire::QueryErrorCode, ResponseUpiu> HandleQueryRequestUpiu(
      QueryRequestUpiu &request);
  std::unique_ptr<UicCommand> CreateUicCommand(UicCommandOpcode opcode,
                                               SendUicCommandRequestView request);

  void ProcessQueryRequestUpiu(const RequestRequestView &request,
                               RequestCompleter::Sync &completer);

  Ufs *const controller_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_SERVER_H_
