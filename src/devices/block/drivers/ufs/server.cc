// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "server.h"

#include <fidl/fuchsia.hardware.ufs/cpp/common_types.h>
#include <fidl/fuchsia.hardware.ufs/cpp/wire_types.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/trace/event.h>

#include "src/devices/block/drivers/ufs/uic/uic_commands.h"
#include "src/devices/block/drivers/ufs/upiu/attributes.h"
#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/flags.h"

namespace ufs {

template <typename ResponseUpiu>
fit::result<QueryErrorCode, ResponseUpiu> UfsServer::HandleQueryRequestUpiu(
    QueryRequestUpiu& request) {
  auto response = controller_->GetTransferRequestProcessor().SendQueryRequestUpiu(request);
  if (response.is_error()) {
    return fit::error(QueryErrorCode::kGeneralFailure);
  }
  if (response->GetHeader().response != UpiuHeaderResponseCode::kTargetSuccess) {
    return fit::error(QueryErrorCode(response->GetHeader().response));
  }
  return fit::ok(std::move(response->GetResponse<ResponseUpiu>()));
}

template fit::result<QueryErrorCode, DescriptorResponseUpiu>
UfsServer::HandleQueryRequestUpiu<DescriptorResponseUpiu>(QueryRequestUpiu& request);

template fit::result<QueryErrorCode, FlagResponseUpiu>
UfsServer::HandleQueryRequestUpiu<FlagResponseUpiu>(QueryRequestUpiu& request);

template fit::result<QueryErrorCode, AttributeResponseUpiu>
UfsServer::HandleQueryRequestUpiu<AttributeResponseUpiu>(QueryRequestUpiu& request);

namespace {
// Extracts the 'type' from the FIDL request object, converts it to an internal type,
// and then extracts the index from the 'identifier' to return it.
// Return value:
//  - On success: Returns a std::pair containing the internal type and the index.
//  - On failure: Returns an appropriate QueryErrorCode error.
// Parameters:
//  - request: The FIDL request object.
//  - max_count: The valid range for the internal type.
template <typename RequestView, typename InternalType>
fit::result<QueryErrorCode, std::pair<InternalType, uint8_t>> GetInternalTypeAndIndex(
    const RequestView& request, InternalType max_count) {
  if (!request.has_type()) {
    FDF_LOG(ERROR, "Invalid FIDL request: missing request type");
    return fit::error(QueryErrorCode::kInvalidIdn);
  }

  // Since the DescriptorType, Flags, and Attributes enums are defined to have the same numeric
  // values as those defined in the UFS Spec for each entry in the FIDL enums DescriptorType,
  // FlagType, and AttributeType, we can convert between them by casting the underlying numeric
  // value.
  uint8_t value = fidl::ToUnderlying(request.type());
  if (value >= static_cast<uint8_t>(max_count)) {
    FDF_LOG(ERROR, "Cannot convert fidl type to internal Type");
    return fit::error(QueryErrorCode::kGeneralFailure);
  }
  uint8_t index = 0;
  if (request.has_identifier() && request.identifier().has_index()) {
    index = request.identifier().index();
  }

  return fit::ok(std::make_pair(static_cast<InternalType>(value), index));
}

}  // namespace

void UfsServer::ReadDescriptor(ReadDescriptorRequestView request,
                               ReadDescriptorCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Descriptor, DescriptorType>(
      request->descriptor, DescriptorType::kDescriptorCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, index] = result.value();
  ReadDescriptorUpiu read_desc_upiu(type, index);
  auto response = HandleQueryRequestUpiu<DescriptorResponseUpiu>(read_desc_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }

  auto data = response.value().GetData<QueryResponseUpiuData>()->command_data;

  fidl::Arena<> arena;
  fidl::VectorView<uint8_t> desc_data(arena, data.begin(), data.end());

  completer.ReplySuccess(desc_data);
}

void UfsServer::WriteDescriptor(WriteDescriptorRequestView request,
                                WriteDescriptorCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Descriptor, DescriptorType>(
      request->descriptor, DescriptorType::kDescriptorCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, index] = result.value();
  WriteDescriptorUpiu write_desc_upiu(type, &request->data, index);
  auto response = HandleQueryRequestUpiu<DescriptorResponseUpiu>(write_desc_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }

  completer.ReplySuccess();
}

void UfsServer::ReadFlag(ReadFlagRequestView request, ReadFlagCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Flag, Flags>(request->flag,
                                                                                 Flags::kFlagCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, _] = result.value();
  ReadFlagUpiu read_flag_upiu(type);
  auto response = HandleQueryRequestUpiu<FlagResponseUpiu>(read_flag_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }

  completer.ReplySuccess(response.value().GetFlag());
}

void UfsServer::SetFlag(SetFlagRequestView request, SetFlagCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Flag, Flags>(request->flag,
                                                                                 Flags::kFlagCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, _] = result.value();
  SetFlagUpiu set_flag_upiu(type);
  auto response = HandleQueryRequestUpiu<FlagResponseUpiu>(set_flag_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }

  completer.ReplySuccess(response.value().GetFlag());
}

void UfsServer::ClearFlag(ClearFlagRequestView request, ClearFlagCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Flag, Flags>(request->flag,
                                                                                 Flags::kFlagCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, _] = result.value();
  ClearFlagUpiu clear_flag_upiu(type);
  auto response = HandleQueryRequestUpiu<FlagResponseUpiu>(clear_flag_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }

  completer.ReplySuccess(response.value().GetFlag());
}

void UfsServer::ToggleFlag(ToggleFlagRequestView request, ToggleFlagCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Flag, Flags>(request->flag,
                                                                                 Flags::kFlagCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, _] = result.value();
  ToggleFlagUpiu toggle_flag_upiu(type);
  auto response = HandleQueryRequestUpiu<FlagResponseUpiu>(toggle_flag_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }

  completer.ReplySuccess(response.value().GetFlag());
}

void UfsServer::ReadAttribute(ReadAttributeRequestView request,
                              ReadAttributeCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Attribute, Attributes>(
      request->attr, Attributes::kAttributeCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, index] = result.value();
  ReadAttributeUpiu read_attr_upiu(type, index);
  auto response = HandleQueryRequestUpiu<AttributeResponseUpiu>(read_attr_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }
  completer.ReplySuccess(response.value().GetAttribute());
}

void UfsServer::WriteAttribute(WriteAttributeRequestView request,
                               WriteAttributeCompleter::Sync& completer) {
  auto result = GetInternalTypeAndIndex<fuchsia_hardware_ufs::wire::Attribute, Attributes>(
      request->attr, Attributes::kAttributeCount);
  if (result.is_error()) {
    completer.Reply(result.take_error());
    return;
  }

  auto [type, index] = result.value();
  WriteAttributeUpiu write_attr_upiu(type, request->value, index);
  auto response = HandleQueryRequestUpiu<AttributeResponseUpiu>(write_attr_upiu);
  if (response.is_error()) {
    completer.Reply(response.take_error());
    return;
  }

  completer.ReplySuccess();
}

void UfsServer::SendUicCommand(SendUicCommandRequestView request,
                               SendUicCommandCompleter::Sync& completer) {
  UicCommandOpcode opcode = static_cast<UicCommandOpcode>(fidl::ToUnderlying(request->opcode));

  std::unique_ptr<UicCommand> command = CreateUicCommand(opcode, request);

  if (!command) {
    FDF_LOG(ERROR, "Unsupported UIC command opcode: 0x%x", opcode);
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  auto result = command->SendCommand();
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to send UicCommand Opcode: 0x%x", opcode);
    completer.ReplyError(result.error_value());
    return;
  }
  uint32_t response = result.value().value_or(0);
  completer.ReplySuccess(response);
}

std::unique_ptr<UicCommand> UfsServer::CreateUicCommand(UicCommandOpcode opcode,
                                                        SendUicCommandRequestView request) {
  uint16_t mib_attribute = (request->argument[0] >> 16) & 0xFFFF;
  uint16_t gen_selector_index = request->argument[0] & 0xFFFF;
  uint8_t attr_set_type = (request->argument[1] >> 16) & 0xFF;
  switch (opcode) {
    case UicCommandOpcode::kDmeGet:
      return std::make_unique<DmeGetUicCommand>(*controller_, mib_attribute, gen_selector_index);
    case UicCommandOpcode::kDmeSet:
      return std::make_unique<DmeSetUicCommand>(*controller_, mib_attribute, gen_selector_index,
                                                attr_set_type, request->argument[2]);
    case UicCommandOpcode::kDmePeerGet:
      return std::make_unique<DmePeerGetUicCommand>(*controller_, mib_attribute,
                                                    gen_selector_index);
    case UicCommandOpcode::kDmePeerSet:
      return std::make_unique<DmePeerSetUicCommand>(*controller_, mib_attribute, gen_selector_index,
                                                    attr_set_type, request->argument[2]);
    default:
      return nullptr;
  }
}

void UfsServer::Request(RequestRequestView request, RequestCompleter::Sync& completer) {
  uint8_t transaction_type = request->request[0];
  if (transaction_type == UpiuTransactionCodes::kQueryRequest) {
    ProcessQueryRequestUpiu(request, completer);
  } else {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
}

void UfsServer::ProcessQueryRequestUpiu(const RequestRequestView& request,
                                        RequestCompleter::Sync& completer) {
  if (request->request.size() != sizeof(QueryRequestUpiuData)) {
    FDF_LOG(ERROR, "Data size mismatch: expected %zu, got %zu", sizeof(QueryRequestUpiuData),
            request->request.size());
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  QueryRequestUpiu query_request_upiu(
      *reinterpret_cast<const QueryRequestUpiuData*>(request->request.data()));
  auto response = controller_->GetTransferRequestProcessor()
                      .SendRequestUpiu<QueryRequestUpiu, QueryResponseUpiu>(query_request_upiu);
  if (response.is_error()) {
    completer.ReplyError(response.error_value());
    return;
  }

  auto response_upiu_data =
      reinterpret_cast<uint8_t*>(response.value()->GetData<QueryResponseUpiuData>());
  fidl::Arena<> arena;
  fidl::VectorView<uint8_t> request_data(arena, response_upiu_data,
                                         response_upiu_data + sizeof(QueryResponseUpiuData));

  completer.ReplySuccess(request_data);
}

}  // namespace ufs
