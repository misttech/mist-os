// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains forwarding stubs from the C entry points to Rust implementations.
//
// Exposed functions TEE_FooBar forwards to api_impl::foo_bar()

#![allow(non_snake_case)]
#![allow(unused_variables)]

use std::unimplemented;
use tee_internal::binding::{
    TEE_Attribute, TEE_BigInt, TEE_BigIntFMM, TEE_BigIntFMMContext, TEE_Identity,
    TEE_ObjectEnumHandle, TEE_ObjectHandle, TEE_ObjectInfo, TEE_OperationHandle, TEE_OperationInfo,
    TEE_OperationInfoMultiple, TEE_Param, TEE_PropSetHandle, TEE_Result, TEE_TASessionHandle,
    TEE_Time, TEE_Whence, TEE_UUID,
};

use crate::mem;

// This function returns a list of the C entry point that we want to expose from
// this program. They need to be referenced from main to ensure that the linker
// thinks that they are referenced and need to be included in the final binary.
pub fn exposed_c_entry_points() -> &'static [*const extern "C" fn()] {
    &[
        TEE_Panic as *const extern "C" fn(),
        TEE_Malloc as *const extern "C" fn(),
        TEE_Realloc as *const extern "C" fn(),
        TEE_Free as *const extern "C" fn(),
        TEE_MemMove as *const extern "C" fn(),
        TEE_MemCompare as *const extern "C" fn(),
        TEE_MemFill as *const extern "C" fn(),
        TEE_GetPropertyAsString as *const extern "C" fn(),
        TEE_GetPropertyAsBool as *const extern "C" fn(),
        TEE_GetPropertyAsU32 as *const extern "C" fn(),
        TEE_GetPropertyAsU64 as *const extern "C" fn(),
        TEE_GetPropertyAsBinaryBlock as *const extern "C" fn(),
        TEE_GetPropertyAsUUID as *const extern "C" fn(),
        TEE_GetPropertyAsIdentity as *const extern "C" fn(),
        TEE_AllocatePropertyEnumerator as *const extern "C" fn(),
        TEE_FreePropertyEnumerator as *const extern "C" fn(),
        TEE_StartPropertyEnumerator as *const extern "C" fn(),
        TEE_ResetPropertyEnumerator as *const extern "C" fn(),
        TEE_GetPropertyName as *const extern "C" fn(),
        TEE_GetNextProperty as *const extern "C" fn(),
        TEE_OpenTASession as *const extern "C" fn(),
        TEE_CloseTASession as *const extern "C" fn(),
        TEE_InvokeTACommand as *const extern "C" fn(),
        TEE_GetCancellationFlag as *const extern "C" fn(),
        TEE_UnmaskCancellation as *const extern "C" fn(),
        TEE_MaskCancellation as *const extern "C" fn(),
        TEE_CheckMemoryAccessRights as *const extern "C" fn(),
        TEE_SetInstanceData as *const extern "C" fn(),
        TEE_GetInstanceData as *const extern "C" fn(),
        TEE_GetObjectInfo1 as *const extern "C" fn(),
        TEE_RestrictObjectUsage1 as *const extern "C" fn(),
        TEE_GetObjectBufferAttribute as *const extern "C" fn(),
        TEE_GetObjectValueAttribute as *const extern "C" fn(),
        TEE_CloseObject as *const extern "C" fn(),
        TEE_AllocateTransientObject as *const extern "C" fn(),
        TEE_FreeTransientObject as *const extern "C" fn(),
        TEE_ResetTransientObject as *const extern "C" fn(),
        TEE_PopulateTransientObject as *const extern "C" fn(),
        TEE_InitRefAttribute as *const extern "C" fn(),
        TEE_InitValueAttribute as *const extern "C" fn(),
        TEE_CopyObjectAttributes1 as *const extern "C" fn(),
        TEE_GenerateKey as *const extern "C" fn(),
        TEE_OpenPersistentObject as *const extern "C" fn(),
        TEE_CreatePersistentObject as *const extern "C" fn(),
        TEE_CloseAndDeletePersistentObject1 as *const extern "C" fn(),
        TEE_RenamePersistentObject as *const extern "C" fn(),
        TEE_AllocatePersistentObjectEnumerator as *const extern "C" fn(),
        TEE_FreePersistentObjectEnumerator as *const extern "C" fn(),
        TEE_ResetPersistentObjectEnumerator as *const extern "C" fn(),
        TEE_StartPersistentObjectEnumerator as *const extern "C" fn(),
        TEE_GetNextPersistentObject as *const extern "C" fn(),
        TEE_ReadObjectData as *const extern "C" fn(),
        TEE_WriteObjectData as *const extern "C" fn(),
        TEE_TruncateObjectData as *const extern "C" fn(),
        TEE_SeekObjectData as *const extern "C" fn(),
        TEE_AllocateOperation as *const extern "C" fn(),
        TEE_FreeOperation as *const extern "C" fn(),
        TEE_GetOperationInfo as *const extern "C" fn(),
        TEE_GetOperationInfoMultiple as *const extern "C" fn(),
        TEE_ResetOperation as *const extern "C" fn(),
        TEE_SetOperationKey as *const extern "C" fn(),
        TEE_SetOperationKey2 as *const extern "C" fn(),
        TEE_CopyOperation as *const extern "C" fn(),
        TEE_IsAlgorithmSupported as *const extern "C" fn(),
        TEE_DigestUpdate as *const extern "C" fn(),
        TEE_DigestDoFinal as *const extern "C" fn(),
        TEE_CipherInit as *const extern "C" fn(),
        TEE_CipherUpdate as *const extern "C" fn(),
        TEE_CipherDoFinal as *const extern "C" fn(),
        TEE_MACInit as *const extern "C" fn(),
        TEE_MACUpdate as *const extern "C" fn(),
        TEE_MACComputeFinal as *const extern "C" fn(),
        TEE_MACCompareFinal as *const extern "C" fn(),
        TEE_AEInit as *const extern "C" fn(),
        TEE_AEUpdateAAD as *const extern "C" fn(),
        TEE_AEUpdate as *const extern "C" fn(),
        TEE_AEEncryptFinal as *const extern "C" fn(),
        TEE_AEDecryptFinal as *const extern "C" fn(),
        TEE_AsymmetricEncrypt as *const extern "C" fn(),
        TEE_AsymmetricDecrypt as *const extern "C" fn(),
        TEE_AsymmetricSignDigest as *const extern "C" fn(),
        TEE_AsymmetricVerifyDigest as *const extern "C" fn(),
        TEE_DeriveKey as *const extern "C" fn(),
        TEE_GenerateRandom as *const extern "C" fn(),
        TEE_GetSystemTime as *const extern "C" fn(),
        TEE_Wait as *const extern "C" fn(),
        TEE_GetTAPersistentTime as *const extern "C" fn(),
        TEE_SetTAPersistentTime as *const extern "C" fn(),
        TEE_GetREETime as *const extern "C" fn(),
        TEE_BigIntFMMContextSizeInU32 as *const extern "C" fn(),
        TEE_BigIntFMMSizeInU32 as *const extern "C" fn(),
        TEE_BigIntInit as *const extern "C" fn(),
        TEE_BigIntInitFMMContext1 as *const extern "C" fn(),
        TEE_BigIntInitFMM as *const extern "C" fn(),
        TEE_BigIntConvertFromOctetString as *const extern "C" fn(),
        TEE_BigIntConvertToOctetString as *const extern "C" fn(),
        TEE_BigIntConvertFromS32 as *const extern "C" fn(),
        TEE_BigIntConvertToS32 as *const extern "C" fn(),
        TEE_BigIntCmp as *const extern "C" fn(),
        TEE_BigIntCmpS32 as *const extern "C" fn(),
        TEE_BigIntShiftRight as *const extern "C" fn(),
        TEE_BigIntGetBit as *const extern "C" fn(),
        TEE_BigIntGetBitCount as *const extern "C" fn(),
        TEE_BigIntSetBit as *const extern "C" fn(),
        TEE_BigIntAssign as *const extern "C" fn(),
        TEE_BigIntAbs as *const extern "C" fn(),
        TEE_BigIntAdd as *const extern "C" fn(),
        TEE_BigIntSub as *const extern "C" fn(),
        TEE_BigIntNeg as *const extern "C" fn(),
        TEE_BigIntMul as *const extern "C" fn(),
        TEE_BigIntSquare as *const extern "C" fn(),
        TEE_BigIntDiv as *const extern "C" fn(),
        TEE_BigIntMod as *const extern "C" fn(),
        TEE_BigIntAddMod as *const extern "C" fn(),
        TEE_BigIntSubMod as *const extern "C" fn(),
        TEE_BigIntMulMod as *const extern "C" fn(),
        TEE_BigIntSquareMod as *const extern "C" fn(),
        TEE_BigIntInvMod as *const extern "C" fn(),
        TEE_BigIntExpMod as *const extern "C" fn(),
        TEE_BigIntRelativePrime as *const extern "C" fn(),
        TEE_BigIntComputeExtendedGcd as *const extern "C" fn(),
        TEE_BigIntIsProbablePrime as *const extern "C" fn(),
        TEE_BigIntConvertToFMM as *const extern "C" fn(),
        TEE_BigIntConvertFromFMM as *const extern "C" fn(),
        TEE_BigIntComputeFMM as *const extern "C" fn(),
        // This function is exposed to configure our default heap allocator.
        mem::__scudo_default_options as *const extern "C" fn(),
    ]
}

#[no_mangle]
pub extern "C" fn TEE_Panic(code: u32) {
    crate::panic(code)
}

#[no_mangle]
extern "C" fn TEE_Malloc(size: usize, hint: u32) -> *mut ::std::os::raw::c_void {
    mem::malloc(size, hint)
}

#[no_mangle]
extern "C" fn TEE_Realloc(
    buffer: *mut ::std::os::raw::c_void,
    newSize: usize,
) -> *mut ::std::os::raw::c_void {
    unsafe { mem::realloc(buffer, newSize) }
}

#[no_mangle]
extern "C" fn TEE_Free(buffer: *mut ::std::os::raw::c_void) {
    unsafe { mem::free(buffer) }
}

#[no_mangle]
extern "C" fn TEE_MemMove(
    dest: *mut ::std::os::raw::c_void,
    src: *mut ::std::os::raw::c_void,
    size: usize,
) {
    mem::mem_move(dest, src, size)
}

#[no_mangle]
extern "C" fn TEE_MemCompare(
    buffer1: *mut ::std::os::raw::c_void,
    buffer2: *mut ::std::os::raw::c_void,
    size: usize,
) -> i32 {
    mem::mem_compare(buffer1, buffer2, size)
}

#[no_mangle]
extern "C" fn TEE_MemFill(buffer: *mut ::std::os::raw::c_void, x: u8, size: usize) {
    mem::mem_fill(buffer, x, size)
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsString(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    valueBuffer: *mut ::std::os::raw::c_char,
    valueBufferLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsBool(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut bool,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsU32(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsU64(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut u64,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsBinaryBlock(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    valueBuffer: *mut ::std::os::raw::c_void,
    valueBufferLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsUUID(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut TEE_UUID,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsIdentity(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut TEE_Identity,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AllocatePropertyEnumerator(enumerator: *mut TEE_PropSetHandle) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_FreePropertyEnumerator(enumerator: TEE_PropSetHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_StartPropertyEnumerator(
    enumerator: TEE_PropSetHandle,
    propSet: TEE_PropSetHandle,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_ResetPropertyEnumerator(enumerator: TEE_PropSetHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyName(
    enumerator: TEE_PropSetHandle,
    nameBuffer: *mut ::std::os::raw::c_void,
    nameBufferLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetNextProperty(enumerator: TEE_PropSetHandle) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_OpenTASession(
    destination: *mut TEE_UUID,
    cancellationRequestTimeout: u32,
    paramTypes: u32,
    params: *mut TEE_Param,
    session: *mut TEE_TASessionHandle,
    returnOrigin: *mut u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CloseTASession(session: TEE_TASessionHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_InvokeTACommand(
    session: TEE_TASessionHandle,
    cancellationRequestTimeout: u32,
    commandID: u32,
    paramTypes: u32,
    params: *mut TEE_Param,
    returnOrigin: *mut u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetCancellationFlag() -> bool {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_UnmaskCancellation() -> bool {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_MaskCancellation() -> bool {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CheckMemoryAccessRights(
    accessFlags: u32,
    buffer: *mut ::std::os::raw::c_void,
    size: usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_SetInstanceData(instanceData: *mut ::std::os::raw::c_void) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetInstanceData() -> *mut ::std::os::raw::c_void {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetObjectInfo1(
    object: TEE_ObjectHandle,
    objectInfo: *mut TEE_ObjectInfo,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_RestrictObjectUsage1(object: TEE_ObjectHandle, objectUsage: u32) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetObjectBufferAttribute(
    object: TEE_ObjectHandle,
    attributeID: u32,
    buffer: *mut ::std::os::raw::c_void,
    size: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetObjectValueAttribute(
    object: TEE_ObjectHandle,
    attributeID: u32,
    a: *mut u32,
    b: *mut u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CloseObject(object: TEE_ObjectHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AllocateTransientObject(
    objectType: u32,
    maxObjectSize: u32,
    object: *mut TEE_ObjectHandle,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_FreeTransientObject(object: TEE_ObjectHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_ResetTransientObject(object: TEE_ObjectHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_PopulateTransientObject(
    object: TEE_ObjectHandle,
    attrs: *mut TEE_Attribute,
    attrCount: u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_InitRefAttribute(
    attr: *mut TEE_Attribute,
    attributeID: u32,
    buffer: *mut ::std::os::raw::c_void,
    length: usize,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_InitValueAttribute(attr: *mut TEE_Attribute, attributeID: u32, a: u32, b: u32) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CopyObjectAttributes1(
    destObject: TEE_ObjectHandle,
    srcObject: TEE_ObjectHandle,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GenerateKey(
    object: TEE_ObjectHandle,
    keySize: u32,
    params: *mut TEE_Attribute,
    paramCount: u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_OpenPersistentObject(
    storageID: u32,
    objectID: *mut ::std::os::raw::c_void,
    objectIDLen: usize,
    flags: u32,
    object: *mut TEE_ObjectHandle,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CreatePersistentObject(
    storageID: u32,
    objectID: *mut ::std::os::raw::c_void,
    objectIDLen: usize,
    flags: u32,
    attributes: TEE_ObjectHandle,
    initialData: *mut ::std::os::raw::c_void,
    initialDataLen: usize,
    object: *mut TEE_ObjectHandle,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CloseAndDeletePersistentObject1(object: TEE_ObjectHandle) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_RenamePersistentObject(
    object: TEE_ObjectHandle,
    newObjectID: *mut ::std::os::raw::c_void,
    newObjectIDLen: usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AllocatePersistentObjectEnumerator(
    objectEnumerator: *mut TEE_ObjectEnumHandle,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_FreePersistentObjectEnumerator(objectEnumerator: TEE_ObjectEnumHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_ResetPersistentObjectEnumerator(objectEnumerator: TEE_ObjectEnumHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_StartPersistentObjectEnumerator(
    objectEnumerator: TEE_ObjectEnumHandle,
    storageID: u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetNextPersistentObject(
    objectEnumerator: TEE_ObjectEnumHandle,
    objectInfo: *mut TEE_ObjectInfo,
    objectID: *mut ::std::os::raw::c_void,
    objectIDLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_ReadObjectData(
    object: TEE_ObjectHandle,
    buffer: *mut ::std::os::raw::c_void,
    size: usize,
    count: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_WriteObjectData(
    object: TEE_ObjectHandle,
    buffer: *mut ::std::os::raw::c_void,
    size: usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_TruncateObjectData(object: TEE_ObjectHandle, size: usize) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_SeekObjectData(
    object: TEE_ObjectHandle,
    offset: usize,
    whence: TEE_Whence,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AllocateOperation(
    operation: *mut TEE_OperationHandle,
    algorithm: u32,
    mode: u32,
    maxKeySize: u32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_FreeOperation(operation: TEE_OperationHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetOperationInfo(
    operation: TEE_OperationHandle,
    operationInfo: *mut TEE_OperationInfo,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetOperationInfoMultiple(
    operation: TEE_OperationHandle,
    operationInfoMultiple: *mut TEE_OperationInfoMultiple,
    operationSize: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_ResetOperation(operation: TEE_OperationHandle) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_SetOperationKey(
    operation: TEE_OperationHandle,
    key: TEE_ObjectHandle,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_SetOperationKey2(
    operation: TEE_OperationHandle,
    key1: TEE_ObjectHandle,
    key2: TEE_ObjectHandle,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CopyOperation(
    dstOperation: TEE_OperationHandle,
    srcOperation: TEE_OperationHandle,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_IsAlgorithmSupported(algId: u32, element: u32) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_DigestUpdate(
    operation: TEE_OperationHandle,
    chunk: *mut ::std::os::raw::c_void,
    chunkSize: usize,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_DigestDoFinal(
    operation: TEE_OperationHandle,
    chunk: *mut ::std::os::raw::c_void,
    chunkLen: usize,
    hash: *mut ::std::os::raw::c_void,
    hashLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CipherInit(
    operation: TEE_OperationHandle,
    IV: *mut ::std::os::raw::c_void,
    IVLen: usize,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CipherUpdate(
    operation: TEE_OperationHandle,
    srcData: *mut ::std::os::raw::c_void,
    srcLen: usize,
    destData: *mut ::std::os::raw::c_void,
    destLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_CipherDoFinal(
    operation: TEE_OperationHandle,
    srcData: *mut ::std::os::raw::c_void,
    srcLen: usize,
    destData: *mut ::std::os::raw::c_void,
    destLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_MACInit(
    operation: TEE_OperationHandle,
    IV: *mut ::std::os::raw::c_void,
    IVLen: usize,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_MACUpdate(
    operation: TEE_OperationHandle,
    chunk: *mut ::std::os::raw::c_void,
    chunkSize: usize,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_MACComputeFinal(
    operation: TEE_OperationHandle,
    message: *mut ::std::os::raw::c_void,
    messageLen: usize,
    mac: *mut ::std::os::raw::c_void,
    macLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_MACCompareFinal(
    operation: TEE_OperationHandle,
    message: *mut ::std::os::raw::c_void,
    messageLen: usize,
    mac: *mut ::std::os::raw::c_void,
    macLen: usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AEInit(
    operation: TEE_OperationHandle,
    nonce: *mut ::std::os::raw::c_void,
    nonceLen: usize,
    tagLen: u32,
    AADLen: usize,
    payloadLen: usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AEUpdateAAD(
    operation: TEE_OperationHandle,
    AADdata: *mut ::std::os::raw::c_void,
    AADdataLen: usize,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AEUpdate(
    operation: TEE_OperationHandle,
    srcData: *mut ::std::os::raw::c_void,
    srcLen: usize,
    destData: *mut ::std::os::raw::c_void,
    destLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AEEncryptFinal(
    operation: TEE_OperationHandle,
    srcData: *mut ::std::os::raw::c_void,
    srcLen: usize,
    destData: *mut ::std::os::raw::c_void,
    destLen: *mut usize,
    tag: *mut ::std::os::raw::c_void,
    tagLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AEDecryptFinal(
    operation: TEE_OperationHandle,
    srcData: *mut ::std::os::raw::c_void,
    srcLen: usize,
    destData: *mut ::std::os::raw::c_void,
    destLen: *mut usize,
    tag: *mut ::std::os::raw::c_void,
    tagLen: usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AsymmetricEncrypt(
    operation: TEE_OperationHandle,
    params: *mut TEE_Attribute,
    paramCount: u32,
    srcData: *mut ::std::os::raw::c_void,
    srcLen: usize,
    destData: *mut ::std::os::raw::c_void,
    destLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AsymmetricDecrypt(
    operation: TEE_OperationHandle,
    params: *mut TEE_Attribute,
    paramCount: u32,
    srcData: *mut ::std::os::raw::c_void,
    srcLen: usize,
    destData: *mut ::std::os::raw::c_void,
    destLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AsymmetricSignDigest(
    operation: TEE_OperationHandle,
    params: *mut TEE_Attribute,
    paramCount: u32,
    digest: *mut ::std::os::raw::c_void,
    digestLen: usize,
    signature: *mut ::std::os::raw::c_void,
    signatureLen: *mut usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_AsymmetricVerifyDigest(
    operation: TEE_OperationHandle,
    params: *mut TEE_Attribute,
    paramCount: u32,
    digest: *mut ::std::os::raw::c_void,
    digestLen: usize,
    signature: *mut ::std::os::raw::c_void,
    signatureLen: usize,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_DeriveKey(
    operation: TEE_OperationHandle,
    params: *mut TEE_Attribute,
    paramCount: u32,
    derivedKey: TEE_ObjectHandle,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GenerateRandom(
    randomBuffer: *mut ::std::os::raw::c_void,
    randomBufferLen: usize,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetSystemTime(time: *mut TEE_Time) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_Wait(timeout: u32) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetTAPersistentTime(time: *mut TEE_Time) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_SetTAPersistentTime(time: *mut TEE_Time) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_GetREETime(time: *mut TEE_Time) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntFMMContextSizeInU32(modulusSizeInBits: usize) -> usize {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntFMMSizeInU32(modulusSizeInBits: usize) -> usize {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntInit(bigInt: *mut TEE_BigInt, len: usize) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntInitFMMContext1(
    context: *mut TEE_BigIntFMMContext,
    len: usize,
    modulus: *mut TEE_BigInt,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntInitFMM(bigIntFMM: *mut TEE_BigIntFMM, len: usize) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntConvertFromOctetString(
    dest: *mut TEE_BigInt,
    buffer: *mut u8,
    bufferLen: usize,
    sign: i32,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntConvertToOctetString(
    buffer: *mut ::std::os::raw::c_void,
    bufferLen: *mut usize,
    bigInt: *mut TEE_BigInt,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntConvertFromS32(dest: *mut TEE_BigInt, shortVal: i32) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntConvertToS32(dest: *mut i32, src: *mut TEE_BigInt) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntCmp(op1: *mut TEE_BigInt, op2: *mut TEE_BigInt) -> i32 {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntCmpS32(op: *mut TEE_BigInt, shortVal: i32) -> i32 {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntShiftRight(dest: *mut TEE_BigInt, op: *mut TEE_BigInt, bits: usize) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntGetBit(src: *mut TEE_BigInt, bitIndex: u32) -> bool {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntGetBitCount(src: *mut TEE_BigInt) -> u32 {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntSetBit(op: *mut TEE_BigInt, bitIndex: u32, value: bool) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntAssign(dest: *mut TEE_BigInt, src: *mut TEE_BigInt) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntAbs(dest: *mut TEE_BigInt, src: *mut TEE_BigInt) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntAdd(dest: *mut TEE_BigInt, op1: *mut TEE_BigInt, op2: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntSub(dest: *mut TEE_BigInt, op1: *mut TEE_BigInt, op2: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntNeg(dest: *mut TEE_BigInt, op: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntMul(dest: *mut TEE_BigInt, op1: *mut TEE_BigInt, op2: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntSquare(dest: *mut TEE_BigInt, op: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntDiv(
    dest_q: *mut TEE_BigInt,
    dest_r: *mut TEE_BigInt,
    op1: *mut TEE_BigInt,
    op2: *mut TEE_BigInt,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntMod(dest: *mut TEE_BigInt, op: *mut TEE_BigInt, n: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntAddMod(
    dest: *mut TEE_BigInt,
    op1: *mut TEE_BigInt,
    op2: *mut TEE_BigInt,
    n: *mut TEE_BigInt,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntSubMod(
    dest: *mut TEE_BigInt,
    op1: *mut TEE_BigInt,
    op2: *mut TEE_BigInt,
    n: *mut TEE_BigInt,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntMulMod(
    dest: *mut TEE_BigInt,
    op1: *mut TEE_BigInt,
    op2: *mut TEE_BigInt,
    n: *mut TEE_BigInt,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntSquareMod(dest: *mut TEE_BigInt, op: *mut TEE_BigInt, n: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntInvMod(dest: *mut TEE_BigInt, op: *mut TEE_BigInt, n: *mut TEE_BigInt) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntExpMod(
    dest: *mut TEE_BigInt,
    op1: *mut TEE_BigInt,
    op2: *mut TEE_BigInt,
    n: *mut TEE_BigInt,
    context: *mut TEE_BigIntFMMContext,
) -> TEE_Result {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntRelativePrime(op1: *mut TEE_BigInt, op2: *mut TEE_BigInt) -> bool {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntComputeExtendedGcd(
    gcd: *mut TEE_BigInt,
    u: *mut TEE_BigInt,
    v: *mut TEE_BigInt,
    op1: *mut TEE_BigInt,
    op2: *mut TEE_BigInt,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntIsProbablePrime(op: *mut TEE_BigInt, confidenceLevel: u32) -> i32 {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntConvertToFMM(
    dest: *mut TEE_BigIntFMM,
    src: *mut TEE_BigInt,
    n: *mut TEE_BigInt,
    context: *mut TEE_BigIntFMMContext,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntConvertFromFMM(
    dest: *mut TEE_BigInt,
    src: *mut TEE_BigIntFMM,
    n: *mut TEE_BigInt,
    context: *mut TEE_BigIntFMMContext,
) {
    unimplemented!()
}

#[no_mangle]
extern "C" fn TEE_BigIntComputeFMM(
    dest: *mut TEE_BigIntFMM,
    op1: *mut TEE_BigIntFMM,
    op2: *mut TEE_BigIntFMM,
    n: *mut TEE_BigInt,
    context: *mut TEE_BigIntFMMContext,
) {
    unimplemented!()
}
