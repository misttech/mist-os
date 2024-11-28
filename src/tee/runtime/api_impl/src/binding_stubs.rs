// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains forwarding stubs from the C entry points to Rust implementations.
//
// Exposed functions TEE_FooBar forwards to api_impl::foo_bar(), where foo_bar deals in the richer
// bindings exposed top-level from the tee_internal crate.

#![allow(non_snake_case)]
#![allow(unused_variables)]

use crate::props::is_propset_pseudo_handle;
use crate::{mem, props, storage};
use num_traits::FromPrimitive;
use tee_internal::binding::{
    TEE_Attribute, TEE_BigInt, TEE_BigIntFMM, TEE_BigIntFMMContext, TEE_Identity,
    TEE_ObjectEnumHandle, TEE_ObjectHandle, TEE_ObjectInfo, TEE_OperationHandle, TEE_OperationInfo,
    TEE_OperationInfoMultiple, TEE_Param, TEE_PropSetHandle, TEE_Result, TEE_TASessionHandle,
    TEE_Time, TEE_Whence, TEE_SUCCESS, TEE_UUID,
};
use tee_internal::{
    to_tee_result, Attribute, AttributeId, Error, HandleFlags, ObjectEnumHandle, ObjectHandle,
    PropSetHandle, Result as TeeResult, Storage, Type, Usage, ValueFields, Whence,
    OBJECT_ID_MAX_LEN,
};

// This function returns a list of the C entry point that we want to expose from
// this program. They need to be referenced from main to ensure that the linker
// thinks that they are referenced and need to be included in the final binary.
//
// Keep in the order as they appear in the spec.
#[rustfmt::skip]
pub fn exposed_c_entry_points() -> &'static [*const extern "C" fn()] {
    &[
        //
        // Trusted Core Framework API
        //

        // Property Access Functions
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

        // Panics
        TEE_Panic as *const extern "C" fn(),

        // Internal Client API
        TEE_OpenTASession as *const extern "C" fn(),
        TEE_CloseTASession as *const extern "C" fn(),
        TEE_InvokeTACommand as *const extern "C" fn(),

        // Cancellation Functions
        TEE_GetCancellationFlag as *const extern "C" fn(),
        TEE_UnmaskCancellation as *const extern "C" fn(),
        TEE_MaskCancellation as *const extern "C" fn(),

        // Memory Management Functions
        TEE_CheckMemoryAccessRights as *const extern "C" fn(),
        TEE_SetInstanceData as *const extern "C" fn(),
        TEE_GetInstanceData as *const extern "C" fn(),
        TEE_Malloc as *const extern "C" fn(),
        TEE_Realloc as *const extern "C" fn(),
        TEE_Free as *const extern "C" fn(),
        TEE_MemMove as *const extern "C" fn(),
        TEE_MemCompare as *const extern "C" fn(),
        TEE_MemFill as *const extern "C" fn(),

        //
        // Trusted Storage API for Data and Keys
        //

        // Generic Object Functions
        TEE_GetObjectInfo1 as *const extern "C" fn(),
        TEE_GetObjectInfo as *const extern "C" fn(),
        TEE_RestrictObjectUsage1 as *const extern "C" fn(),
        TEE_RestrictObjectUsage as *const extern "C" fn(),
        TEE_GetObjectBufferAttribute as *const extern "C" fn(),
        TEE_GetObjectValueAttribute as *const extern "C" fn(),
        TEE_CloseObject as *const extern "C" fn(),

        // Transient Object Functions
        TEE_AllocateTransientObject as *const extern "C" fn(),
        TEE_FreeTransientObject as *const extern "C" fn(),
        TEE_ResetTransientObject as *const extern "C" fn(),
        TEE_PopulateTransientObject as *const extern "C" fn(),
        TEE_InitRefAttribute as *const extern "C" fn(),
        TEE_InitValueAttribute as *const extern "C" fn(),
        TEE_CopyObjectAttributes1 as *const extern "C" fn(),
        TEE_CopyObjectAttributes as *const extern "C" fn(),
        TEE_GenerateKey as *const extern "C" fn(),

        // Persistent Object Functions
        TEE_OpenPersistentObject as *const extern "C" fn(),
        TEE_CreatePersistentObject as *const extern "C" fn(),
        TEE_CloseAndDeletePersistentObject1 as *const extern "C" fn(),
        TEE_CloseAndDeletePersistentObject as *const extern "C" fn(),
        TEE_RenamePersistentObject as *const extern "C" fn(),

        // Persistent Object Enumeration Functions
        TEE_AllocatePersistentObjectEnumerator as *const extern "C" fn(),
        TEE_FreePersistentObjectEnumerator as *const extern "C" fn(),
        TEE_ResetPersistentObjectEnumerator as *const extern "C" fn(),
        TEE_StartPersistentObjectEnumerator as *const extern "C" fn(),
        TEE_GetNextPersistentObject as *const extern "C" fn(),

        // Data Stream Access Functions
        TEE_ReadObjectData as *const extern "C" fn(),
        TEE_WriteObjectData as *const extern "C" fn(),
        TEE_TruncateObjectData as *const extern "C" fn(),
        TEE_SeekObjectData as *const extern "C" fn(),

        //
        // Cryptographic Operations API
        //

        // Generic Options Functions
        TEE_AllocateOperation as *const extern "C" fn(),
        TEE_FreeOperation as *const extern "C" fn(),
        TEE_GetOperationInfo as *const extern "C" fn(),
        TEE_GetOperationInfoMultiple as *const extern "C" fn(),
        TEE_ResetOperation as *const extern "C" fn(),
        TEE_SetOperationKey as *const extern "C" fn(),
        TEE_SetOperationKey2 as *const extern "C" fn(),
        TEE_CopyOperation as *const extern "C" fn(),
        TEE_IsAlgorithmSupported as *const extern "C" fn(),

        // Message Digest Functions
        TEE_DigestUpdate as *const extern "C" fn(),
        TEE_DigestDoFinal as *const extern "C" fn(),
        TEE_DigestExtract as *const extern "C" fn(),

        // Symmetric Cipher Functions
        TEE_CipherInit as *const extern "C" fn(),
        TEE_CipherUpdate as *const extern "C" fn(),
        TEE_CipherDoFinal as *const extern "C" fn(),

        // MAC Functions
        TEE_MACInit as *const extern "C" fn(),
        TEE_MACUpdate as *const extern "C" fn(),
        TEE_MACComputeFinal as *const extern "C" fn(),
        TEE_MACCompareFinal as *const extern "C" fn(),

        // Authenticated Encryption Functions
        TEE_AEInit as *const extern "C" fn(),
        TEE_AEUpdateAAD as *const extern "C" fn(),
        TEE_AEUpdate as *const extern "C" fn(),
        TEE_AEEncryptFinal as *const extern "C" fn(),
        TEE_AEDecryptFinal as *const extern "C" fn(),

        // Asymmmetric Functions
        TEE_AsymmetricEncrypt as *const extern "C" fn(),
        TEE_AsymmetricDecrypt as *const extern "C" fn(),
        TEE_AsymmetricSignDigest as *const extern "C" fn(),
        TEE_AsymmetricVerifyDigest as *const extern "C" fn(),

        // Key Derivation Functions
        TEE_DeriveKey as *const extern "C" fn(),

        // Random Data Generation Functions
        TEE_GenerateRandom as *const extern "C" fn(),

        //
        // Time API
        //

        // Time Functions
        TEE_GetSystemTime as *const extern "C" fn(),
        TEE_Wait as *const extern "C" fn(),
        TEE_GetTAPersistentTime as *const extern "C" fn(),
        TEE_SetTAPersistentTime as *const extern "C" fn(),
        TEE_GetREETime as *const extern "C" fn(),

        //
        // TEE Arithmetical API
        //

        // Memory Allocation and Size of Objects
        TEE_BigIntFMMContextSizeInU32 as *const extern "C" fn(),
        TEE_BigIntFMMSizeInU32 as *const extern "C" fn(),

        // Initialization Functions
        TEE_BigIntInit as *const extern "C" fn(),
        TEE_BigIntInitFMMContext1 as *const extern "C" fn(),
        TEE_BigIntInitFMM as *const extern "C" fn(),

        // Converter Functions
        TEE_BigIntConvertFromOctetString as *const extern "C" fn(),
        TEE_BigIntConvertToOctetString as *const extern "C" fn(),
        TEE_BigIntConvertFromS32 as *const extern "C" fn(),
        TEE_BigIntConvertToS32 as *const extern "C" fn(),

        // Logical Operations
        TEE_BigIntCmp as *const extern "C" fn(),
        TEE_BigIntCmpS32 as *const extern "C" fn(),
        TEE_BigIntShiftRight as *const extern "C" fn(),
        TEE_BigIntGetBit as *const extern "C" fn(),
        TEE_BigIntGetBitCount as *const extern "C" fn(),
        TEE_BigIntSetBit as *const extern "C" fn(),
        TEE_BigIntAssign as *const extern "C" fn(),
        TEE_BigIntAbs as *const extern "C" fn(),

        // Basic Arithmetic Operations
        TEE_BigIntAdd as *const extern "C" fn(),
        TEE_BigIntSub as *const extern "C" fn(),
        TEE_BigIntNeg as *const extern "C" fn(),
        TEE_BigIntMul as *const extern "C" fn(),
        TEE_BigIntSquare as *const extern "C" fn(),
        TEE_BigIntDiv as *const extern "C" fn(),

        // Modular Arithmetic Operations
        TEE_BigIntMod as *const extern "C" fn(),
        TEE_BigIntAddMod as *const extern "C" fn(),
        TEE_BigIntSubMod as *const extern "C" fn(),
        TEE_BigIntMulMod as *const extern "C" fn(),
        TEE_BigIntSquareMod as *const extern "C" fn(),
        TEE_BigIntInvMod as *const extern "C" fn(),
        TEE_BigIntExpMod as *const extern "C" fn(),

        // Other Arithmetic Operations
        TEE_BigIntRelativePrime as *const extern "C" fn(),
        TEE_BigIntComputeExtendedGcd as *const extern "C" fn(),
        TEE_BigIntIsProbablePrime as *const extern "C" fn(),

        // Fast Modular Multiplication Operations
        TEE_BigIntConvertToFMM as *const extern "C" fn(),
        TEE_BigIntConvertFromFMM as *const extern "C" fn(),
        TEE_BigIntComputeFMM as *const extern "C" fn(),

        //
        // Additional
        //

        // This function is exposed to configure our default heap allocator.
        mem::__scudo_default_options as *const extern "C" fn(),
    ]
}

fn slice_from_raw_parts_mut<'a, Input, Output>(data: *mut Input, size: usize) -> &'a mut [Output] {
    debug_assert_eq!(align_of::<Input>(), align_of::<Output>());
    debug_assert_eq!(size_of::<Input>(), size_of::<Output>());
    if data.is_null() {
        assert_eq!(size, 0);
        &mut []
    } else {
        // SAFETY: `data` is non-null in this branch, and the library must
        // assume that it points to valid memory.

        assert!(data.is_aligned());
        assert!(
            size * size_of::<Input>() < isize::MAX.try_into().unwrap(),
            "Size of buf slice is too large and will cause undefined behavior"
        );
        // SAFETY: According to the safety concerns for `std::slice::from_raw_parts_mut`:
        // [1] data must be [valid] for both reads and writes for len * mem::size_of::<T>() many bytes, and it must be properly aligned.
        // [2] The entire memory range of this slice must be contained within a single allocated object
        // [3] data must be non-null and aligned even for zero-length slices
        // [4] data must point to len consecutive properly initialized values of type T.
        // [5] The memory referenced by the returned slice must not be accessed through any other pointer
        // [6] The total size len * mem::size_of::<T>() of the slice must be no larger than isize::MAX,
        //      and adding that size to data must not "wrap around" the address space.
        //
        // Nullity, alignment, and size are checked above, satisfying [3] and parts of [1] and [6].
        // [1] (validity), [2], [4], [5], and [6] (wrap-around) are the responsibility of the caller to uphold.
        unsafe { std::slice::from_raw_parts_mut(data as *mut Output, size) }
    }
}

fn slice_from_raw_parts<'a, Input, Output>(data: *const Input, size: usize) -> &'a [Output] {
    debug_assert_eq!(align_of::<Input>(), align_of::<Output>());
    debug_assert_eq!(size_of::<Input>(), size_of::<Output>());
    if data.is_null() {
        assert_eq!(size, 0);
        &mut []
    } else {
        assert!(data.is_aligned());
        assert!(
            size * size_of::<Input>() < isize::MAX.try_into().unwrap(),
            "Size of buf slice is too large and will cause undefined behavior"
        );
        // SAFETY: According to the safety concerns for `std::slice::from_raw_parts_mut`:
        // [1] data must be [valid] for reads for len * mem::size_of::<T>() many bytes, and it must be properly aligned.
        // [2] The entire memory range of this slice must be contained within a single allocated object
        // [3] data must be non-null and aligned even for zero-length slices
        // [4] data must point to len consecutive properly initialized values of type T.
        // [5] The memory referenced by the returned slice  not be mutated for the duration of lifetime 'a, except inside an UnsafeCell.
        // [6] The total size len * mem::size_of::<T>() of the slice must be no larger than isize::MAX,
        //      and adding that size to data must not "wrap around" the address space.
        //
        // Nullity, alignment, and size are checked above, satisfying [3] and parts of [1] and [6].
        // [1] (validity), [2], [4], [5], and [6] (wrap-around) are the responsibility of the caller to uphold.
        unsafe { std::slice::from_raw_parts(data as *const Output, size) }
    }
}

// Returns None if a Utf8Error is encountered.
fn c_str_to_str<'a>(name: *const ::std::os::raw::c_char) -> Option<&'a str> {
    assert!(!name.is_null());
    // SAFETY: According to the safety concerns for `CStr::from_ptr`:
    // [1] The memory pointed to by ptr must contain a valid nul terminator at the end of the string.
    // [2] ptr must be [valid] for reads of bytes up to and including the nul terminator. This means in particular:
    //     [2a] The entire memory range of this CStr must be contained within a single allocated object!
    //     [2b] ptr must be non-null even for a zero-length cstr.
    // [3] The memory referenced by the returned CStr must not be mutated for the duration of lifetime 'a.
    // [4] The nul terminator must be within isize::MAX from ptr
    //
    // [1], [2a], and [4] are assumed to be upheld by the caller, and not checked here.
    // [2b] is checked above for nullity, and we do not mutate the memory here, satisfying [3].
    let name_cstr = unsafe { std::ffi::CStr::from_ptr(name) };
    name_cstr.to_str().ok()
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsString(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    valueBuffer: *mut ::std::os::raw::c_char,
    valueBufferLen: *mut usize,
) -> TEE_Result {
    assert!(!valueBuffer.is_null());
    assert!(valueBuffer.is_aligned());
    assert!(!valueBufferLen.is_null());
    assert!(valueBufferLen.is_aligned());
    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&propsetOrEnumerator);
        let name = if is_propset_pseudo_handle(handle) {
            c_str_to_str(name).ok_or(Error::ItemNotFound)?
        } else {
            ""
        };

        // SAFETY: Nullity and alignment are checked above, but full validity of the memory read
        // is the responsibility of the caller (e.g. out-of-bounds or freed memory pointers).
        let initial_buf_len = unsafe { *valueBufferLen };
        let mut buf = slice_from_raw_parts_mut(valueBuffer, initial_buf_len);

        let (len, result) = match props::get_property_as_string(handle, name, &mut buf) {
            Ok(written) => {
                // written.len() does not include the NUL terminator byte, so cases where we
                // write the exact buffer length are captured by `<` rather than `<=`.
                debug_assert!(written.len() < initial_buf_len);
                (written.len(), Ok(()))
            }
            Err(err) => {
                if err.error == Error::ShortBuffer {
                    (err.actual_length, Err(err.error))
                } else {
                    (err.written.len(), Err(err.error))
                }
            }
        };

        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            // Add 1 for NUL terminator.
            *valueBufferLen = len + 1;
        }

        result
    })())
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsBool(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut bool,
) -> TEE_Result {
    assert!(!value.is_null());
    assert!(value.is_aligned());

    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&propsetOrEnumerator);
        let name = if is_propset_pseudo_handle(handle) {
            c_str_to_str(name).ok_or(Error::ItemNotFound)?
        } else {
            ""
        };
        let val = props::get_property_as_bool(handle, name)?;
        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            *value = val;
        }
        Ok(())
    })())
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsU32(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut u32,
) -> TEE_Result {
    assert!(!value.is_null());
    assert!(value.is_aligned());

    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&propsetOrEnumerator);
        let name = if is_propset_pseudo_handle(handle) {
            c_str_to_str(name).ok_or(Error::ItemNotFound)?
        } else {
            ""
        };
        let val = props::get_property_as_u32(handle, name)?;
        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            *value = val;
        }
        Ok(())
    })())
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsU64(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut u64,
) -> TEE_Result {
    assert!(!value.is_null());
    assert!(value.is_aligned());

    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&propsetOrEnumerator);
        let name = if is_propset_pseudo_handle(handle) {
            c_str_to_str(name).ok_or(Error::ItemNotFound)?
        } else {
            ""
        };
        let val = props::get_property_as_u64(handle, name)?;
        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            *value = val;
        }
        Ok(())
    })())
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsBinaryBlock(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    valueBuffer: *mut ::std::os::raw::c_void,
    valueBufferLen: *mut usize,
) -> TEE_Result {
    assert!(!valueBufferLen.is_null());
    assert!(valueBufferLen.is_aligned());

    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&propsetOrEnumerator);
        let name = if is_propset_pseudo_handle(handle) {
            c_str_to_str(name).ok_or(Error::ItemNotFound)?
        } else {
            ""
        };
        // SAFETY: Nullity and alignment are checked above, but full validity of the memory read
        // is the responsibility of the caller (e.g. out-of-bounds or freed memory pointers).
        let initial_buf_len = unsafe { *valueBufferLen };
        let mut buf = slice_from_raw_parts_mut(valueBuffer as *mut u8, initial_buf_len);

        let (len, result) = match props::get_property_as_binary_block(handle, name, &mut buf) {
            Ok(bytes_written) => {
                debug_assert!(bytes_written.len() <= initial_buf_len);
                (bytes_written.len(), Ok(()))
            }
            Err(err) => {
                let len = match err.error {
                    Error::ShortBuffer => err.actual_length,
                    Error::BadFormat => 0,
                    _ => err.written.len(),
                };
                (len, Err(err.error))
            }
        };

        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            *valueBufferLen = len;
        };

        result
    })())
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsUUID(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut TEE_UUID,
) -> TEE_Result {
    assert!(!value.is_null());
    assert!(value.is_aligned());

    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&propsetOrEnumerator);
        let name = if is_propset_pseudo_handle(handle) {
            c_str_to_str(name).ok_or(Error::ItemNotFound)?
        } else {
            ""
        };
        let uuid = props::get_property_as_uuid(handle, name)?;
        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            *value = *uuid.to_binding();
        }
        Ok(())
    })())
}

#[no_mangle]
extern "C" fn TEE_GetPropertyAsIdentity(
    propsetOrEnumerator: TEE_PropSetHandle,
    name: *mut ::std::os::raw::c_char,
    value: *mut TEE_Identity,
) -> TEE_Result {
    assert!(!value.is_null());
    assert!(value.is_aligned());

    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&propsetOrEnumerator);
        let name = if is_propset_pseudo_handle(handle) {
            c_str_to_str(name).ok_or(Error::ItemNotFound)?
        } else {
            ""
        };
        let identity = props::get_property_as_identity(handle, name)?;
        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            *value = *identity.to_binding();
        }
        Ok(())
    })())
}

#[no_mangle]
extern "C" fn TEE_AllocatePropertyEnumerator(enumerator: *mut TEE_PropSetHandle) -> TEE_Result {
    assert!(!enumerator.is_null());
    assert!(enumerator.is_aligned());
    let handle = props::allocate_property_enumerator();
    // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
    // other validity concerns for `ptr::write()`.
    unsafe {
        *enumerator = handle.to_binding().clone();
    }
    TEE_SUCCESS
}

#[no_mangle]
extern "C" fn TEE_FreePropertyEnumerator(enumerator: TEE_PropSetHandle) {
    props::free_property_enumerator(*PropSetHandle::from_binding(&enumerator));
}

#[no_mangle]
extern "C" fn TEE_StartPropertyEnumerator(
    enumerator: TEE_PropSetHandle,
    propSet: TEE_PropSetHandle,
) {
    props::start_property_enumerator(
        *PropSetHandle::from_binding(&enumerator),
        *PropSetHandle::from_binding(&propSet),
    );
}

#[no_mangle]
extern "C" fn TEE_ResetPropertyEnumerator(enumerator: TEE_PropSetHandle) {
    props::reset_property_enumerator(*PropSetHandle::from_binding(&enumerator));
}

#[no_mangle]
extern "C" fn TEE_GetPropertyName(
    enumerator: TEE_PropSetHandle,
    nameBuffer: *mut ::std::os::raw::c_void,
    nameBufferLen: *mut usize,
) -> TEE_Result {
    assert!(!nameBuffer.is_null());
    assert!(nameBuffer.is_aligned());
    assert!(!nameBufferLen.is_null());
    assert!(nameBufferLen.is_aligned());

    to_tee_result((|| -> TeeResult {
        let handle = *PropSetHandle::from_binding(&enumerator);

        // SAFETY: Nullity and alignment are checked above, but full validity of the memory read
        // is the responsibility of the caller (e.g. out-of-bounds or freed memory pointers).
        let initial_buf_len = unsafe { *nameBufferLen };
        let mut buf = slice_from_raw_parts_mut(nameBuffer, initial_buf_len);

        let (len, result) = match props::get_property_name(handle, &mut buf) {
            Ok(written) => {
                // written.len() does not include the NUL terminator byte, so cases where we
                // write the exact buffer length are captured by `<` rather than `<=`.
                debug_assert!(written.len() < initial_buf_len);
                (written.len(), Ok(()))
            }
            Err(err) => {
                if err.error == Error::ShortBuffer {
                    (err.actual_length, Err(err.error))
                } else {
                    (err.written.len(), Err(err.error))
                }
            }
        };

        // SAFETY: Nullity and alignment are checked above. The caller is responsible for upholding
        // other validity concerns for `ptr::write()`.
        unsafe {
            // Add 1 for NUL terminator.
            *nameBufferLen = len + 1;
        }

        result
    })())
}

#[no_mangle]
extern "C" fn TEE_GetNextProperty(enumerator: TEE_PropSetHandle) -> TEE_Result {
    to_tee_result((|| -> TeeResult {
        props::get_next_property(*PropSetHandle::from_binding(&enumerator))
    })())
}

#[no_mangle]
pub extern "C" fn TEE_Panic(code: u32) {
    crate::panic(code)
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
    // TODO(https://fxbug.dev/370103570): Implement Cancellation APIs.
    return true;
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
    assert!(!objectInfo.is_null());
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let info = storage::get_object_info(object);
        // SAFETY: `objectInfo` nullity checked above.
        unsafe {
            *objectInfo = *info.to_binding();
        }
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_GetObjectInfo(object: TEE_ObjectHandle, objectInfo: *mut TEE_ObjectInfo) {
    assert_eq!(TEE_GetObjectInfo1(object, objectInfo), TEE_SUCCESS);
}

#[no_mangle]
extern "C" fn TEE_RestrictObjectUsage1(object: TEE_ObjectHandle, objectUsage: u32) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let usage = Usage::from_bits_retain(objectUsage);
        storage::restrict_object_usage(object, usage);
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_RestrictObjectUsage(object: TEE_ObjectHandle, objectUsage: u32) {
    assert_eq!(TEE_RestrictObjectUsage1(object, objectUsage), TEE_SUCCESS);
}

#[no_mangle]
extern "C" fn TEE_GetObjectBufferAttribute(
    object: TEE_ObjectHandle,
    attributeID: u32,
    buffer: *mut ::std::os::raw::c_void,
    size: *mut usize,
) -> TEE_Result {
    assert!(!size.is_null());
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let id = AttributeId::from_u32(attributeID).unwrap();
        // SAFETY: `size` nullity checked above.
        let initial_size = unsafe { *size };
        let buffer = slice_from_raw_parts_mut(buffer, initial_size);
        let (attribute_size, result) =
            match storage::get_object_buffer_attribute(object, id, buffer) {
                Ok(written) => {
                    debug_assert!(written.len() <= initial_size);
                    (written.len(), Ok(()))
                }
                Err(err) => (err.actual_size, Err(err.error)),
            };
        // SAFETY: `size` nullity checked above.
        unsafe {
            *size = attribute_size;
        }
        result
    }())
}

#[no_mangle]
extern "C" fn TEE_GetObjectValueAttribute(
    object: TEE_ObjectHandle,
    attributeID: u32,
    a: *mut u32,
    b: *mut u32,
) -> TEE_Result {
    assert!(!a.is_null());
    assert!(!b.is_null());
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let id = AttributeId::from_u32(attributeID).unwrap();
        let val = storage::get_object_value_attribute(object, id)?;
        // SAFETY: `a` and `b` nullity checked above.
        unsafe {
            (*a, *b) = (val.a, val.b);
        }
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_CloseObject(object: TEE_ObjectHandle) {
    let object = *ObjectHandle::from_binding(&object);
    storage::close_object(object);
}

#[no_mangle]
extern "C" fn TEE_AllocateTransientObject(
    objectType: u32,
    maxObjectSize: u32,
    object: *mut TEE_ObjectHandle,
) -> TEE_Result {
    assert!(!object.is_null());
    to_tee_result(|| -> TeeResult {
        let object_type = Type::from_u32(objectType).unwrap();
        let obj = storage::allocate_transient_object(object_type, maxObjectSize)?;
        // SAFETY: `object` nullity checked above.
        unsafe {
            *object = *obj.to_binding();
        }
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_FreeTransientObject(object: TEE_ObjectHandle) {
    let object = *ObjectHandle::from_binding(&object);
    storage::free_transient_object(object);
}

#[no_mangle]
extern "C" fn TEE_ResetTransientObject(object: TEE_ObjectHandle) {
    let object = *ObjectHandle::from_binding(&object);
    storage::reset_transient_object(object);
}

#[no_mangle]
extern "C" fn TEE_PopulateTransientObject(
    object: TEE_ObjectHandle,
    attrs: *mut TEE_Attribute,
    attrCount: u32,
) -> TEE_Result {
    // SAFETY: check that the TEE_Attribute entries do indeed give
    // bitwise-valid Atttibute instances before recasting below.
    for raw_attr in
        slice_from_raw_parts::<TEE_Attribute, TEE_Attribute>(attrs, attrCount as usize).iter()
    {
        assert!(Attribute::from_binding(&raw_attr).is_some());
    }

    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let attrs = slice_from_raw_parts(attrs, attrCount as usize);
        storage::populate_transient_object(object, attrs as &[Attribute])
    }())
}

#[no_mangle]
extern "C" fn TEE_InitRefAttribute(
    attr: *mut TEE_Attribute,
    attributeID: u32,
    buffer: *mut ::std::os::raw::c_void,
    length: usize,
) {
    assert!(!attr.is_null());
    let id = AttributeId::from_u32(attributeID).unwrap();
    let buffer = slice_from_raw_parts_mut(buffer, length);
    let attribute = storage::init_ref_attribute(id, buffer);
    // SAFETY: `attr` nullity checked above.
    unsafe { *attr = *attribute.to_binding() };
}

#[no_mangle]
extern "C" fn TEE_InitValueAttribute(attr: *mut TEE_Attribute, attributeID: u32, a: u32, b: u32) {
    assert!(!attr.is_null());
    let id = AttributeId::from_u32(attributeID).unwrap();
    let attribute = storage::init_value_attribute(id, ValueFields { a, b });
    // SAFETY: `attr` nullity checked above.
    unsafe { *attr = *attribute.to_binding() };
}

#[no_mangle]
extern "C" fn TEE_CopyObjectAttributes1(
    destObject: TEE_ObjectHandle,
    srcObject: TEE_ObjectHandle,
) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let src = *ObjectHandle::from_binding(&srcObject);
        let dest = *ObjectHandle::from_binding(&destObject);
        storage::copy_object_attributes(src, dest)
    }())
}

#[no_mangle]
extern "C" fn TEE_CopyObjectAttributes(destObject: TEE_ObjectHandle, srcObject: TEE_ObjectHandle) {
    assert_eq!(TEE_CopyObjectAttributes1(destObject, srcObject), TEE_SUCCESS);
}

#[no_mangle]
extern "C" fn TEE_GenerateKey(
    object: TEE_ObjectHandle,
    keySize: u32,
    params: *mut TEE_Attribute,
    paramCount: u32,
) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let params = slice_from_raw_parts(params, paramCount as usize);
        storage::generate_key(object, keySize, params)
    }())
}

#[no_mangle]
extern "C" fn TEE_OpenPersistentObject(
    storageID: u32,
    objectID: *mut ::std::os::raw::c_void,
    objectIDLen: usize,
    flags: u32,
    object: *mut TEE_ObjectHandle,
) -> TEE_Result {
    assert!(!object.is_null());
    to_tee_result(|| -> TeeResult {
        let storage = Storage::from_u32(storageID).ok_or(Error::ItemNotFound)?;
        let flags = HandleFlags::from_bits_retain(flags);
        let id = slice_from_raw_parts(objectID, objectIDLen);
        let obj = storage::open_persistent_object(storage, id, flags)?;
        // SAFETY: `object` nullity checked above.
        unsafe {
            *object = *obj.to_binding();
        }
        Ok(())
    }())
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
    to_tee_result(|| -> TeeResult {
        let storage = Storage::from_u32(storageID).ok_or(Error::ItemNotFound)?;
        let flags = HandleFlags::from_bits_retain(flags);
        let id = slice_from_raw_parts(objectID, objectIDLen);
        let attrs = *ObjectHandle::from_binding(&attributes);
        let initial_data = slice_from_raw_parts(initialData, initialDataLen);
        let obj = storage::create_persistent_object(storage, id, flags, attrs, initial_data)?;
        if object.is_null() {
            // The user doesn't want a handle, so just close the newly minted one.
            storage::close_object(obj);
        } else {
            // SAFETY: `object` is non-null in this branch.
            unsafe {
                *object = *obj.to_binding();
            }
        }
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_CloseAndDeletePersistentObject1(object: TEE_ObjectHandle) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        storage::close_and_delete_persistent_object(object)
    }())
}

#[no_mangle]
extern "C" fn TEE_CloseAndDeletePersistentObject(object: TEE_ObjectHandle) {
    assert_eq!(TEE_CloseAndDeletePersistentObject1(object), TEE_SUCCESS);
}

#[no_mangle]
extern "C" fn TEE_RenamePersistentObject(
    object: TEE_ObjectHandle,
    newObjectID: *mut ::std::os::raw::c_void,
    newObjectIDLen: usize,
) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let new_id = slice_from_raw_parts(newObjectID, newObjectIDLen);
        storage::rename_persistent_object(object, new_id)
    }())
}

#[no_mangle]
extern "C" fn TEE_AllocatePersistentObjectEnumerator(
    objectEnumerator: *mut TEE_ObjectEnumHandle,
) -> TEE_Result {
    assert!(!objectEnumerator.is_null());
    to_tee_result(|| -> TeeResult {
        let enumerator = storage::allocate_persistent_object_enumerator();
        // SAFETY: `objectEnumerator` nullity checked above.
        unsafe {
            *objectEnumerator = *enumerator.to_binding();
        }
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_FreePersistentObjectEnumerator(objectEnumerator: TEE_ObjectEnumHandle) {
    let enumerator = *ObjectEnumHandle::from_binding(&objectEnumerator);
    storage::free_persistent_object_enumerator(enumerator);
}

#[no_mangle]
extern "C" fn TEE_ResetPersistentObjectEnumerator(objectEnumerator: TEE_ObjectEnumHandle) {
    let enumerator = *ObjectEnumHandle::from_binding(&objectEnumerator);
    storage::reset_persistent_object_enumerator(enumerator);
}

#[no_mangle]
extern "C" fn TEE_StartPersistentObjectEnumerator(
    objectEnumerator: TEE_ObjectEnumHandle,
    storageID: u32,
) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let enumerator = *ObjectEnumHandle::from_binding(&objectEnumerator);
        let storage = Storage::from_u32(storageID).ok_or(Error::ItemNotFound)?;
        storage::start_persistent_object_enumerator(enumerator, storage)
    }())
}

#[no_mangle]
extern "C" fn TEE_GetNextPersistentObject(
    objectEnumerator: TEE_ObjectEnumHandle,
    objectInfo: *mut TEE_ObjectInfo,
    objectID: *mut ::std::os::raw::c_void,
    objectIDLen: *mut usize,
) -> TEE_Result {
    assert!(!objectID.is_null());
    assert!(!objectIDLen.is_null());
    to_tee_result(|| -> TeeResult {
        let enumerator = *ObjectEnumHandle::from_binding(&objectEnumerator);
        let id_buf = slice_from_raw_parts_mut(objectID, OBJECT_ID_MAX_LEN);
        let (info, id) = storage::get_next_persistent_object(enumerator, id_buf)?;
        // SAFETY: `objectIDLen` nullity checked above.
        unsafe {
            *objectIDLen = id.len();
        }
        if !objectInfo.is_null() {
            // SAFETY" `objectInfo` is non-null in this branch.
            unsafe {
                *objectInfo = *info.to_binding();
            }
        }
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_ReadObjectData(
    object: TEE_ObjectHandle,
    buffer: *mut ::std::os::raw::c_void,
    size: usize,
    count: *mut usize,
) -> TEE_Result {
    assert!(!count.is_null());
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let buffer = slice_from_raw_parts_mut(buffer, size);
        let written = storage::read_object_data(object, buffer)?;
        // SAFETY: `count` nullity checked above.
        unsafe {
            *count = written.len();
        }
        Ok(())
    }())
}

#[no_mangle]
extern "C" fn TEE_WriteObjectData(
    object: TEE_ObjectHandle,
    buffer: *mut ::std::os::raw::c_void,
    size: usize,
) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let buffer = slice_from_raw_parts(buffer, size);
        storage::write_object_data(object, buffer)
    }())
}

#[no_mangle]
extern "C" fn TEE_TruncateObjectData(object: TEE_ObjectHandle, size: usize) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        storage::truncate_object_data(object, size)
    }())
}

#[no_mangle]
extern "C" fn TEE_SeekObjectData(
    object: TEE_ObjectHandle,
    offset: std::os::raw::c_long,
    whence: TEE_Whence,
) -> TEE_Result {
    to_tee_result(|| -> TeeResult {
        let object = *ObjectHandle::from_binding(&object);
        let whence = Whence::from_u32(whence).unwrap();
        storage::seek_data_object(object, offset.try_into().unwrap(), whence)
    }())
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
extern "C" fn TEE_DigestExtract(
    operation: TEE_OperationHandle,
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
extern "C" fn TEE_BigIntInitFMMContext(
    context: *mut TEE_BigIntFMMContext,
    len: usize,
    modulus: *mut TEE_BigInt,
) {
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
