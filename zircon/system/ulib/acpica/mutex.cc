// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <ctime>

#include <kernel/mutex.h>

#include "zircon/system/ulib/acpica/oszircon.h"

struct AcpiMutex : public Mutex {
  AcpiMutex() : Mutex() {}
};

struct AcpiSpinLock : public SpinLock {
  AcpiSpinLock() : SpinLock() {}
};

/**
 * @brief Create a mutex.
 *
 * @param OutHandle A pointer to a locaton where a handle to the mutex is
 *        to be returned.
 *
 * @return AE_OK The mutex was successfully created.
 * @return AE_BAD_PARAMETER The OutHandle pointer is NULL.
 * @return AE_NO_MEMORY Insufficient memory to create the mutex.
 */
ACPI_STATUS AcpiOsCreateMutex(ACPI_MUTEX* OutHandle) {
  fbl::AllocChecker ac;
  AcpiMutex* mutex = new (&ac) AcpiMutex();
  if (!ac.check()) {
    return AE_NO_MEMORY;
  }
  *OutHandle = mutex;
  return AE_OK;
}

/**
 * @brief Delete a mutex.
 *
 * @param Handle A handle to a mutex objected that was returned by a
 *        previous call to AcpiOsCreateMutex.
 */
void AcpiOsDeleteMutex(ACPI_MUTEX Handle) { delete (Mutex*)Handle; }

/**
 * @brief Acquire a mutex.
 *
 * @param Handle A handle to a mutex objected that was returned by a
 *        previous call to AcpiOsCreateMutex.
 * @param Timeout How long the caller is willing to wait for the requested
 *        units, in milliseconds.  A value of -1 indicates that the caller
 *        is willing to wait forever. Timeout may be 0.
 *
 * @return AE_OK The requested units were successfully received.
 * @return AE_BAD_PARAMETER The Handle is invalid.
 * @return AE_TIME The mutex could not be acquired within the specified time.
 */
ACPI_STATUS AcpiOsAcquireMutex(ACPI_MUTEX Handle, UINT16 Timeout)
    TA_TRY_ACQ(/*AE_OK*/ 0, Handle) TA_NO_THREAD_SAFETY_ANALYSIS {
  // In the TA_TRY_ACQ statement above we would have liked to use the AE_OK constant, however due to
  // the way it is defined it has a cast in it, and this can result in the clang analysis not always
  // believing it is a literal value. The solution is to place the value 0 directly in TA_TRY_ACQ
  // and then have this static_assert to ensure it's the correct value.
  static_assert(AE_OK == 0);
  ((AcpiMutex*)Handle)->Acquire(ZX_MSEC(Timeout));
  return AE_OK;
}

/**
 * @brief Release a mutex.
 *
 * @param Handle A handle to a mutex objected that was returned by a
 *        previous call to AcpiOsCreateMutex.
 */
void AcpiOsReleaseMutex(ACPI_MUTEX Handle) TA_REL(Handle) { ((AcpiMutex*)Handle)->Release(); }

/**
 * @brief Create a spin lock.
 *
 * @param OutHandle A pointer to a locaton where a handle to the lock is
 *        to be returned.
 *
 * @return AE_OK The lock was successfully created.
 * @return AE_BAD_PARAMETER The OutHandle pointer is NULL.
 * @return AE_NO_MEMORY Insufficient memory to create the lock.
 */
ACPI_STATUS AcpiOsCreateLock(ACPI_SPINLOCK* OutHandle) {
  fbl::AllocChecker ac;
  AcpiSpinLock* spinlock = new (&ac) AcpiSpinLock();
  if (!ac.check()) {
    return AE_NO_MEMORY;
  }
  *OutHandle = spinlock;
  return AE_OK;
}

/**
 * @brief Delete a spin lock.
 *
 * @param Handle A handle to a lock objected that was returned by a
 *        previous call to AcpiOsCreateLock.
 */
void AcpiOsDeleteLock(ACPI_SPINLOCK Handle) { delete (SpinLock*)Handle; }

/**
 * @brief Acquire a spin lock.
 *
 * @param Handle A handle to a lock objected that was returned by a
 *        previous call to AcpiOsCreateLock.
 *
 * @return Platform-dependent CPU flags.  To be used when the lock is released.
 */
ACPI_CPU_FLAGS AcpiOsAcquireLock(ACPI_SPINLOCK Handle) TA_ACQ(Handle) {
  interrupt_saved_state_t irqstate;
  ((AcpiSpinLock*)Handle)->AcquireIrqSave(irqstate);
  return irqstate;
}

/**
 * @brief Release a spin lock.
 *
 * @param Handle A handle to a lock objected that was returned by a
 *        previous call to AcpiOsCreateLock.
 * @param Flags CPU Flags that were returned from AcpiOsAcquireLock.
 */
void AcpiOsReleaseLock(ACPI_SPINLOCK Handle, ACPI_CPU_FLAGS Flags) TA_REL(Handle) {
  ((AcpiSpinLock*)Handle)->ReleaseIrqRestore(Flags);
}
