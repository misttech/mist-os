// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use {
    crate::mode_management::{Defect, EventHistory, IfaceFailure, PhyFailure},
    tracing::warn,
};

// As a general note, recovery is intended to be a method of last resort.  It should be used in
// circumstances where it is thought that WLAN firmware or the interface with the WLAN peripheral
// are not working properly.

// To ensure that devices are not constantly recovering, throttle recovery interventions by
// ensuring that PHY resets are only recommended every 24 hours and interface destructions are only
// recommended every 12 hours.
const HOURS_BETWEEN_PHY_RESETS: i64 = 24;
const HOURS_BETWEEN_IFACE_DESTRUCTIONS: i64 = 12;

// The following constants were empirically determined by looking over aggregate fleet metrics of
// device-day counts of events that represent unexpected device behavior.  Devices are allowed to
// encounter a number of events up to these thresholds before some recovery intervention may be
// recommended.
const SCAN_FAILURE_RECOVERY_THRESHOLD: usize = 5;
const EMPTY_SCAN_RECOVERY_THRESHOLD: usize = 10;
const CONNECT_FAILURE_RECOVERY_THRESHOLD: usize = 15;
const AP_START_FAILURE_RECOVERY_THRESHOLD: usize = 12;
const CREATE_IFACE_FAILURE_RECOVERY_THRESHOLD: usize = 1;
const DESTROY_IFACE_FAILURE_RECOVERY_THRESHOLD: usize = 1;

#[derive(Clone, Copy, Debug)]
pub enum PhyRecoveryOperation {
    #[allow(unused)]
    DestroyIface { iface_id: u16 },
    #[allow(unused)]
    ResetPhy { phy_id: u16 },
}

impl PartialEq for PhyRecoveryOperation {
    fn eq(&self, other: &Self) -> bool {
        match *self {
            PhyRecoveryOperation::DestroyIface { .. } => {
                matches!(*other, PhyRecoveryOperation::DestroyIface { .. })
            }
            PhyRecoveryOperation::ResetPhy { .. } => {
                matches!(*other, PhyRecoveryOperation::ResetPhy { .. })
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum IfaceRecoveryOperation {
    #[allow(unused)]
    Disconnect { iface_id: u16 },
    #[allow(unused)]
    StopAp { iface_id: u16 },
}

impl PartialEq for IfaceRecoveryOperation {
    fn eq(&self, other: &Self) -> bool {
        match *self {
            IfaceRecoveryOperation::Disconnect { .. } => {
                matches!(*other, IfaceRecoveryOperation::Disconnect { .. })
            }
            IfaceRecoveryOperation::StopAp { .. } => {
                matches!(*other, IfaceRecoveryOperation::StopAp { .. })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecoveryAction {
    PhyRecovery(PhyRecoveryOperation),
    IfaceRecovery(IfaceRecoveryOperation),
}

// The purpose of a RecoveryProfile function is to look at the most-recently observed defect in the
// context of past defects that have been encountered and past recovery actions that have been
// taken and suggest a possible recovery action to take to remedy the most recent defect.
pub type RecoveryProfile = fn(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    latest_defect: Defect,
) -> Option<RecoveryAction>;

// This is available so that new products' behaviors can be characterized before enforcing any
// recovery thresholds.  This will enable finding real bugs in a device's behavior.
#[allow(unused)]
fn recovery_disabled(
    _phy_id: u16,
    _defect_history: &mut EventHistory<Defect>,
    _recovery_history: &mut EventHistory<RecoveryAction>,
    _latest_defect: Defect,
) -> Option<RecoveryAction> {
    None
}

// Enable the lookup of recovery profiles by description.
#[allow(unused)]
pub(crate) fn lookup_recovery_profile(profile_name: &str) -> RecoveryProfile {
    match profile_name {
        "" => recovery_disabled,
        other => {
            warn!("Invalid recovery profile: {}.  Proceeding with default.", other);
            recovery_disabled
        }
    }
}

fn thresholded_iface_destruction_and_phy_reset(
    phy_id: u16,
    iface_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    most_recent_defect: Defect,
    defect_count_threshold: usize,
) -> Option<RecoveryAction> {
    // These are the potential remedies that this function will recommend.
    let proposed_phy_reset_action =
        RecoveryAction::PhyRecovery(PhyRecoveryOperation::ResetPhy { phy_id });
    let proposed_iface_destruction_action =
        RecoveryAction::PhyRecovery(PhyRecoveryOperation::DestroyIface { iface_id });

    // If the threshold has not been crossed, don't recommend any recovery.
    if defect_history.event_count(most_recent_defect) < defect_count_threshold {
        return None;
    }

    // If interface destruction has already been attempted and further defects are observed,
    // suggest performing PHY recovery.  This should only be allowed once every 24 hours.
    if recovery_history.event_count(proposed_iface_destruction_action) > 0 {
        let phy_reset_allowed =
            match recovery_history.time_since_last_event(proposed_phy_reset_action) {
                None => true,
                Some(time) => time.into_hours() > HOURS_BETWEEN_PHY_RESETS,
            };

        if phy_reset_allowed {
            return Some(proposed_phy_reset_action);
        }
    }

    // After the threshold has been crossed, attempt to clear any bad state by destroying the
    // interface.  This should be allowed once every 12 hours and should be the first thing to try
    // to get the desired operation to work again.
    let destroy_iface_allowed =
        match recovery_history.time_since_last_event(proposed_iface_destruction_action) {
            None => true,
            Some(time) => time.into_hours() > HOURS_BETWEEN_IFACE_DESTRUCTIONS,
        };

    if destroy_iface_allowed {
        return Some(proposed_iface_destruction_action);
    }

    None
}

fn thresholded_phy_reset(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    most_recent_defect: Defect,
    defect_count_threshold: usize,
) -> Option<RecoveryAction> {
    let proposed_phy_reset_action =
        RecoveryAction::PhyRecovery(PhyRecoveryOperation::ResetPhy { phy_id });

    if defect_history.event_count(most_recent_defect) < defect_count_threshold {
        return None;
    }

    // If the threshold has been crossed and sufficient time has passed since the last PHY reset,
    // recommend that the PHY be reset.
    let recovery_allowed = match recovery_history.time_since_last_event(proposed_phy_reset_action) {
        None => true,
        Some(time) => time.into_hours() > HOURS_BETWEEN_PHY_RESETS,
    };

    if recovery_allowed {
        return Some(proposed_phy_reset_action);
    }

    None
}

fn thresholded_scan_failure_recovery_profile(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    scan_failure_defect: Defect,
) -> Option<RecoveryAction> {
    let iface_id = match scan_failure_defect {
        Defect::Iface(IfaceFailure::FailedScan { iface_id }) => iface_id,
        other => {
            warn!("Assessing invalid defect type for scan failure recovery: {:?}", other);
            return None;
        }
    };

    thresholded_iface_destruction_and_phy_reset(
        phy_id,
        iface_id,
        defect_history,
        recovery_history,
        scan_failure_defect,
        SCAN_FAILURE_RECOVERY_THRESHOLD,
    )
}

fn thresholded_empty_scan_results_recovery_profile(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    empty_scan_defect: Defect,
) -> Option<RecoveryAction> {
    let iface_id = match empty_scan_defect {
        Defect::Iface(IfaceFailure::EmptyScanResults { iface_id }) => iface_id,
        other => {
            warn!("Assessing invalid defect type for empty scan results recovery: {:?}", other);
            return None;
        }
    };

    thresholded_iface_destruction_and_phy_reset(
        phy_id,
        iface_id,
        defect_history,
        recovery_history,
        empty_scan_defect,
        EMPTY_SCAN_RECOVERY_THRESHOLD,
    )
}

fn thresholded_connect_failure_recovery_profile(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    connect_defect: Defect,
) -> Option<RecoveryAction> {
    let iface_id = match connect_defect {
        Defect::Iface(IfaceFailure::ConnectionFailure { iface_id }) => iface_id,
        other => {
            warn!("Assessing invalid defect type for connection failure recovery: {:?}", other);
            return None;
        }
    };

    thresholded_iface_destruction_and_phy_reset(
        phy_id,
        iface_id,
        defect_history,
        recovery_history,
        connect_defect,
        CONNECT_FAILURE_RECOVERY_THRESHOLD,
    )
}

fn thresholded_ap_start_failure_recovery_profile(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    ap_start_defect: Defect,
) -> Option<RecoveryAction> {
    match ap_start_defect {
        Defect::Iface(IfaceFailure::ApStartFailure { .. }) => thresholded_phy_reset(
            phy_id,
            defect_history,
            recovery_history,
            ap_start_defect,
            AP_START_FAILURE_RECOVERY_THRESHOLD,
        ),
        other => {
            warn!("Assessing invalid defect type for AP start failure recovery: {:?}", other);
            None
        }
    }
}

fn thresholded_create_iface_failure_recovery_profile(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    create_iface_defect: Defect,
) -> Option<RecoveryAction> {
    match create_iface_defect {
        Defect::Phy(PhyFailure::IfaceCreationFailure { .. }) => thresholded_phy_reset(
            phy_id,
            defect_history,
            recovery_history,
            create_iface_defect,
            CREATE_IFACE_FAILURE_RECOVERY_THRESHOLD,
        ),
        other => {
            warn!("Assessing invalid defect type for create iface failure recovery: {:?}", other);
            None
        }
    }
}

fn thresholded_destroy_iface_failure_recovery_profile(
    phy_id: u16,
    defect_history: &mut EventHistory<Defect>,
    recovery_history: &mut EventHistory<RecoveryAction>,
    destroy_iface_defect: Defect,
) -> Option<RecoveryAction> {
    match destroy_iface_defect {
        Defect::Phy(PhyFailure::IfaceDestructionFailure { .. }) => thresholded_phy_reset(
            phy_id,
            defect_history,
            recovery_history,
            destroy_iface_defect,
            DESTROY_IFACE_FAILURE_RECOVERY_THRESHOLD,
        ),
        other => {
            warn!("Assessing invalid defect type for destroy iface failure recovery: {:?}", other);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        fuchsia_async::{TestExecutor, Time},
        fuchsia_zircon::DurationNum,
        rand::Rng,
    };

    #[fuchsia::test]
    fn test_recovery_action_equality() {
        let mut rng = rand::thread_rng();
        assert_eq!(
            RecoveryAction::PhyRecovery(PhyRecoveryOperation::DestroyIface {
                iface_id: rng.gen::<u16>()
            }),
            RecoveryAction::PhyRecovery(PhyRecoveryOperation::DestroyIface {
                iface_id: rng.gen::<u16>()
            }),
        );
        assert_eq!(
            RecoveryAction::PhyRecovery(PhyRecoveryOperation::ResetPhy {
                phy_id: rng.gen::<u16>()
            }),
            RecoveryAction::PhyRecovery(PhyRecoveryOperation::ResetPhy {
                phy_id: rng.gen::<u16>()
            }),
        );
        assert_eq!(
            RecoveryAction::IfaceRecovery(IfaceRecoveryOperation::Disconnect {
                iface_id: rng.gen::<u16>()
            }),
            RecoveryAction::IfaceRecovery(IfaceRecoveryOperation::Disconnect {
                iface_id: rng.gen::<u16>()
            }),
        );
        assert_eq!(
            RecoveryAction::IfaceRecovery(IfaceRecoveryOperation::StopAp {
                iface_id: rng.gen::<u16>()
            }),
            RecoveryAction::IfaceRecovery(IfaceRecoveryOperation::StopAp {
                iface_id: rng.gen::<u16>()
            }),
        );
    }

    const PHY_ID: u16 = 123;
    const IFACE_ID: u16 = 456;

    // This test verifies that:
    // 1. No recovery is suggested until the failure recovery threshold is crossed.
    // 2. That interface destruction is suggested prior to PHY reset.
    // 3. Once interface destruction recovery is performed, it is not recommended until the time
    //    limit has elapsed.
    // 4. Once PHY reset has been suggested, it is not recommended again until the time limit has
    //    elapsed.
    fn test_thresholded_iface_destruction_and_phy_reset(
        exec: &TestExecutor,
        recovery_fn: RecoveryProfile,
        defect_to_log: Defect,
        defect_threshold: usize,
    ) {
        // Set the test time to start at time zero.
        let start_time = Time::from_nanos(0);
        exec.set_fake_time(start_time);

        // These are the potential recovery interventions that will be recommended.
        let destroy_iface_recommendation =
            RecoveryAction::PhyRecovery(PhyRecoveryOperation::DestroyIface { iface_id: IFACE_ID });
        let reset_phy_recommendation =
            RecoveryAction::PhyRecovery(PhyRecoveryOperation::ResetPhy { phy_id: PHY_ID });

        // Retain defects and recovery actions for 48 hours.
        let mut defects = EventHistory::<Defect>::new(48 * 60 * 60);
        let mut recoveries = EventHistory::<RecoveryAction>::new(48 * 60 * 60);

        // Add failures until just under the threshold.
        for _ in 0..(defect_threshold - 1) {
            defects.add_event(defect_to_log);

            // Verify that there is no recovery recommended
            assert_eq!(None, recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,));
        }

        // Add one more failure and verify that a destroy iface was recommended.
        defects.add_event(defect_to_log);
        assert_eq!(
            Some(destroy_iface_recommendation),
            recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,)
        );

        // Record the recovery action and then log another failure to verify that a PHY reset
        // is recommended.
        recoveries.add_event(destroy_iface_recommendation);
        defects.add_event(defect_to_log);
        assert_eq!(
            Some(reset_phy_recommendation),
            recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,)
        );

        // Record the PHY reset and then advance the clock 11 hours and make sure that no recovery
        // is recommended.
        //
        // This is now 11 hours past the test start time.
        recoveries.add_event(reset_phy_recommendation);
        exec.set_fake_time(Time::after(11.hours()));
        defects.add_event(defect_to_log);
        assert_eq!(None, recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,));

        // Advance the clock another 2 hours to ensure that the time between iface destruction
        // recovery recommendations has elapsed.
        //
        // This is now 13 hours past the start of the test.
        exec.set_fake_time(Time::after(2.hours()));
        defects.add_event(defect_to_log);
        assert_eq!(
            Some(destroy_iface_recommendation),
            recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,)
        );

        // Record the destroy iface recovery action and advance the clock 10 more hours and make
        // sure that no recovery is recommended.
        //
        // This is now 23 hours past the start of the test.
        recoveries.add_event(destroy_iface_recommendation);
        exec.set_fake_time(Time::after(10.hours()));
        defects.add_event(defect_to_log);
        assert_eq!(None, recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,));

        // Advance the clock another 2 hours to ensure that the time between PHY resets has elapsed.
        //
        // This is now 25 hours past the start of the test.
        exec.set_fake_time(Time::after(2.hours()));
        defects.add_event(defect_to_log);
        assert_eq!(
            Some(reset_phy_recommendation),
            recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,)
        );
    }

    // This test verifies that:
    // 1. No recovery is recommended until the threshold has been crossed.
    // 2. PHY reset is recommended once the threshold is crossed.
    // 3. PHY resets are only recommended once per 24-hour period.
    fn test_thresholded_phy_reset(
        exec: &TestExecutor,
        recovery_fn: RecoveryProfile,
        defect_to_log: Defect,
        defect_threshold: usize,
    ) {
        // Set the test time to start at time zero.
        let start_time = Time::from_nanos(0);
        exec.set_fake_time(start_time);

        // The PHY recovery intervention that is expected.
        let reset_phy_recommendation =
            RecoveryAction::PhyRecovery(PhyRecoveryOperation::ResetPhy { phy_id: PHY_ID });

        // Retain defects and recovery actions for 48 hours.
        let mut defects = EventHistory::<Defect>::new(48 * 60 * 60);
        let mut recoveries = EventHistory::<RecoveryAction>::new(48 * 60 * 60);

        // Add failures until just under the threshold.
        for _ in 0..(defect_threshold - 1) {
            defects.add_event(defect_to_log);

            // Verify that there is no recovery recommended
            assert_eq!(None, recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,));
        }

        // Add one more failure and verify that a PHY reset was recommended.
        defects.add_event(defect_to_log);
        assert_eq!(
            Some(reset_phy_recommendation),
            recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,)
        );
        recoveries.add_event(reset_phy_recommendation);

        // Add another defect and verify that no recovery is recommended.
        defects.add_event(defect_to_log);
        assert_eq!(None, recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,));

        // Advance the clock 23 hours, log another defect, and verify no recovery is recommended.
        exec.set_fake_time(Time::after(23.hours()));
        defects.add_event(defect_to_log);
        assert_eq!(None, recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,));

        // Advance the clock another 2 hours to get beyond the 24 hour throttle and verify that
        // another occurrence of the defect results in a PHY reset recovery recommendation.
        exec.set_fake_time(Time::after(23.hours()));
        defects.add_event(defect_to_log);
        assert_eq!(
            Some(reset_phy_recommendation),
            recovery_fn(PHY_ID, &mut defects, &mut recoveries, defect_to_log,)
        );
    }

    #[fuchsia::test]
    fn test_scan_failure_recovery() {
        let exec = TestExecutor::new_with_fake_time();
        let defect_to_log = Defect::Iface(IfaceFailure::FailedScan { iface_id: IFACE_ID });
        test_thresholded_iface_destruction_and_phy_reset(
            &exec,
            thresholded_scan_failure_recovery_profile,
            defect_to_log,
            SCAN_FAILURE_RECOVERY_THRESHOLD,
        );
    }

    #[fuchsia::test]
    fn test_empty_scan_results_recovery() {
        let exec = TestExecutor::new_with_fake_time();
        let defect_to_log = Defect::Iface(IfaceFailure::EmptyScanResults { iface_id: IFACE_ID });
        test_thresholded_iface_destruction_and_phy_reset(
            &exec,
            thresholded_empty_scan_results_recovery_profile,
            defect_to_log,
            EMPTY_SCAN_RECOVERY_THRESHOLD,
        );
    }

    #[fuchsia::test]
    fn test_connect_failure_recovery() {
        let exec = TestExecutor::new_with_fake_time();
        let defect_to_log = Defect::Iface(IfaceFailure::ConnectionFailure { iface_id: IFACE_ID });
        test_thresholded_iface_destruction_and_phy_reset(
            &exec,
            thresholded_connect_failure_recovery_profile,
            defect_to_log,
            CONNECT_FAILURE_RECOVERY_THRESHOLD,
        );
    }

    #[fuchsia::test]
    fn test_ap_start_failure_recovery() {
        let exec = TestExecutor::new_with_fake_time();
        let defect_to_log = Defect::Iface(IfaceFailure::ApStartFailure { iface_id: IFACE_ID });
        test_thresholded_phy_reset(
            &exec,
            thresholded_ap_start_failure_recovery_profile,
            defect_to_log,
            AP_START_FAILURE_RECOVERY_THRESHOLD,
        )
    }

    #[fuchsia::test]
    fn test_create_iface_failure_recovery() {
        let exec = TestExecutor::new_with_fake_time();
        let defect_to_log = Defect::Phy(PhyFailure::IfaceCreationFailure { phy_id: PHY_ID });
        test_thresholded_phy_reset(
            &exec,
            thresholded_create_iface_failure_recovery_profile,
            defect_to_log,
            CREATE_IFACE_FAILURE_RECOVERY_THRESHOLD,
        )
    }

    #[fuchsia::test]
    fn test_destroy_iface_failure_recovery() {
        let exec = TestExecutor::new_with_fake_time();
        let defect_to_log = Defect::Phy(PhyFailure::IfaceDestructionFailure { phy_id: PHY_ID });
        test_thresholded_phy_reset(
            &exec,
            thresholded_destroy_iface_failure_recovery_profile,
            defect_to_log,
            DESTROY_IFACE_FAILURE_RECOVERY_THRESHOLD,
        )
    }
}
