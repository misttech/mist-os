// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
    vec::Vec,
};
use core::fmt::Debug;

use assert_matches::assert_matches;
use derivative::Derivative;
use packet_formats::ip::IpExt;

use crate::{Action, Hook, IpRoutines, NatRoutines, Routine, Routines, Rule, UninstalledRoutine};

/// Provided filtering state was invalid.
#[derive(Derivative, Debug)]
#[cfg_attr(test, derivative(PartialEq(bound = "RuleInfo: PartialEq")))]
pub enum ValidationError<RuleInfo> {
    /// A rule matches on a property that is unavailable in the context in which it
    /// will be evaluated. For example, matching on the input interface in the
    /// EGRESS hook.
    RuleWithInvalidMatcher(RuleInfo),
}

/// Witness type ensuring that the contained filtering state has been validated.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct ValidRoutines<I: IpExt, DeviceClass>(Routines<I, DeviceClass, ()>);

impl<I: IpExt, DeviceClass> ValidRoutines<I, DeviceClass> {
    /// Accesses the inner state.
    pub fn get(&self) -> &Routines<I, DeviceClass, ()> {
        let Self(state) = self;
        &state
    }
}

impl<I: IpExt, DeviceClass: Clone + Debug> ValidRoutines<I, DeviceClass> {
    /// Validates the provide state and creates a new `ValidRoutines` along with a
    /// list of all uninstalled routines that are referred to from an installed
    /// routine. Returns a `ValidationError` if the state is invalid.
    ///
    /// The provided state must not contain any cyclical routine graphs (formed by
    /// rules with jump actions). The behavior in this case is unspecified but could
    /// be a deadlock or a panic, for example.
    ///
    /// # Panics
    ///
    /// Panics if the provided state includes cyclic routine graphs.
    pub fn new<RuleInfo: Clone>(
        routines: Routines<I, DeviceClass, RuleInfo>,
    ) -> Result<(Self, Vec<UninstalledRoutine<I, DeviceClass, ()>>), ValidationError<RuleInfo>>
    {
        let Routines { ip: ip_routines, nat: nat_routines } = &routines;

        // Ensure that no rule has a matcher that is unavailable in the context in which
        // the rule will be evaluated.
        let IpRoutines { ingress, local_ingress, egress, local_egress, forwarding: _ } =
            ip_routines;
        validate_hook(&ingress, UnavailableMatcher::OutInterface)?;
        validate_hook(&local_ingress, UnavailableMatcher::OutInterface)?;
        validate_hook(&egress, UnavailableMatcher::InInterface)?;
        validate_hook(&local_egress, UnavailableMatcher::InInterface)?;

        let NatRoutines { ingress, local_ingress, egress, local_egress } = nat_routines;
        validate_hook(&ingress, UnavailableMatcher::OutInterface)?;
        validate_hook(&local_ingress, UnavailableMatcher::OutInterface)?;
        validate_hook(&egress, UnavailableMatcher::InInterface)?;
        validate_hook(&local_egress, UnavailableMatcher::InInterface)?;

        // TODO(https://fxbug.dev/318717702): ensure that no rule has an action
        // that is not valid for the routine and hook to which the rule is
        // appended. For example, NAT rules are not allowed outside of NAT
        // routines, and the TPROXY action is only allowed in the INGRESS hook.

        let mut index = UninstalledRoutineIndex::default();
        let routines = routines.strip_debug_info(&mut index);
        Ok((Self(routines), index.into_values()))
    }
}

#[derive(Clone, Copy)]
enum UnavailableMatcher {
    InInterface,
    OutInterface,
}

/// Ensures that no rules reachable from this hook match on
/// `unavailable_matcher`.
fn validate_hook<I: IpExt, DeviceClass, RuleInfo: Clone>(
    Hook { routines }: &Hook<I, DeviceClass, RuleInfo>,
    unavailable_matcher: UnavailableMatcher,
) -> Result<(), ValidationError<RuleInfo>> {
    for routine in routines {
        validate_routine(routine, unavailable_matcher)?;
    }

    Ok(())
}

/// Ensures that no rules reachable from this routine match on
/// `unavailable_matcher`.
fn validate_routine<I: IpExt, DeviceClass, RuleInfo: Clone>(
    Routine { rules }: &Routine<I, DeviceClass, RuleInfo>,
    unavailable_matcher: UnavailableMatcher,
) -> Result<(), ValidationError<RuleInfo>> {
    for Rule { matcher, action, validation_info } in rules {
        let matcher = match unavailable_matcher {
            UnavailableMatcher::InInterface => &matcher.in_interface,
            UnavailableMatcher::OutInterface => &matcher.out_interface,
        };
        if matcher.is_some() {
            return Err(ValidationError::RuleWithInvalidMatcher(validation_info.clone()));
        }
        match action {
            Action::Accept | Action::Drop | Action::Return => {}
            Action::Jump(target) => {
                let UninstalledRoutine { routine, id: _ } = target;
                validate_routine(&*routine, unavailable_matcher)?
            }
        }
    }

    Ok(())
}

#[derive(Derivative, Debug)]
#[derivative(PartialEq(bound = ""))]
enum ConvertedRoutine<I: IpExt, DeviceClass> {
    InProgress,
    Done(UninstalledRoutine<I, DeviceClass, ()>),
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
struct UninstalledRoutineIndex<I: IpExt, DeviceClass, RuleInfo> {
    index: HashMap<UninstalledRoutine<I, DeviceClass, RuleInfo>, ConvertedRoutine<I, DeviceClass>>,
}

impl<I: IpExt, DeviceClass: Clone + Debug + Debug, RuleInfo: Clone>
    UninstalledRoutineIndex<I, DeviceClass, RuleInfo>
{
    fn get_or_insert_with(
        &mut self,
        target: UninstalledRoutine<I, DeviceClass, RuleInfo>,
        convert: impl FnOnce(
            &mut UninstalledRoutineIndex<I, DeviceClass, RuleInfo>,
        ) -> UninstalledRoutine<I, DeviceClass, ()>,
    ) -> UninstalledRoutine<I, DeviceClass, ()> {
        match self.index.entry(target.clone()) {
            Entry::Occupied(entry) => match entry.get() {
                ConvertedRoutine::InProgress => panic!("cycle in routine graph"),
                ConvertedRoutine::Done(routine) => return routine.clone(),
            },
            Entry::Vacant(entry) => {
                let _ = entry.insert(ConvertedRoutine::InProgress);
            }
        }
        // Convert the target routine and store it in the index, so that the next time
        // we attempt to convert it, we just reuse the already-converted routine.
        let converted = convert(self);
        let previous = self.index.insert(target, ConvertedRoutine::Done(converted.clone()));
        assert_eq!(previous, Some(ConvertedRoutine::InProgress));
        converted
    }

    fn into_values(self) -> Vec<UninstalledRoutine<I, DeviceClass, ()>> {
        self.index
            .into_values()
            .map(|routine| assert_matches!(routine, ConvertedRoutine::Done(routine) => routine))
            .collect()
    }
}

impl<I: IpExt, DeviceClass: Clone + Debug, RuleInfo: Clone> Routines<I, DeviceClass, RuleInfo> {
    fn strip_debug_info(
        self,
        index: &mut UninstalledRoutineIndex<I, DeviceClass, RuleInfo>,
    ) -> Routines<I, DeviceClass, ()> {
        let Self { ip: ip_routines, nat: nat_routines } = self;
        Routines {
            ip: ip_routines.strip_debug_info(index),
            nat: nat_routines.strip_debug_info(index),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone + Debug, RuleInfo: Clone> IpRoutines<I, DeviceClass, RuleInfo> {
    fn strip_debug_info(
        self,
        index: &mut UninstalledRoutineIndex<I, DeviceClass, RuleInfo>,
    ) -> IpRoutines<I, DeviceClass, ()> {
        let Self { ingress, local_ingress, egress, local_egress, forwarding } = self;
        IpRoutines {
            ingress: ingress.strip_debug_info(index),
            local_ingress: local_ingress.strip_debug_info(index),
            forwarding: forwarding.strip_debug_info(index),
            egress: egress.strip_debug_info(index),
            local_egress: local_egress.strip_debug_info(index),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone + Debug, RuleInfo: Clone> NatRoutines<I, DeviceClass, RuleInfo> {
    fn strip_debug_info(
        self,
        index: &mut UninstalledRoutineIndex<I, DeviceClass, RuleInfo>,
    ) -> NatRoutines<I, DeviceClass, ()> {
        let Self { ingress, local_ingress, egress, local_egress } = self;
        NatRoutines {
            ingress: ingress.strip_debug_info(index),
            local_ingress: local_ingress.strip_debug_info(index),
            egress: egress.strip_debug_info(index),
            local_egress: local_egress.strip_debug_info(index),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone + Debug, RuleInfo: Clone> Hook<I, DeviceClass, RuleInfo> {
    fn strip_debug_info(
        self,
        index: &mut UninstalledRoutineIndex<I, DeviceClass, RuleInfo>,
    ) -> Hook<I, DeviceClass, ()> {
        let Self { routines } = self;
        Hook {
            routines: routines.into_iter().map(|routine| routine.strip_debug_info(index)).collect(),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone + Debug, RuleInfo: Clone> Routine<I, DeviceClass, RuleInfo> {
    fn strip_debug_info(
        self,
        index: &mut UninstalledRoutineIndex<I, DeviceClass, RuleInfo>,
    ) -> Routine<I, DeviceClass, ()> {
        let Self { rules } = self;
        Routine {
            rules: rules
                .into_iter()
                .map(|Rule { matcher, action, validation_info: _ }| Rule {
                    matcher,
                    action: action.strip_debug_info(index),
                    validation_info: (),
                })
                .collect(),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone + Debug, RuleInfo: Clone> Action<I, DeviceClass, RuleInfo> {
    fn strip_debug_info(
        self,
        index: &mut UninstalledRoutineIndex<I, DeviceClass, RuleInfo>,
    ) -> Action<I, DeviceClass, ()> {
        match self {
            Self::Accept => Action::Accept,
            Self::Drop => Action::Drop,
            Self::Return => Action::Return,
            Self::Jump(target) => {
                let converted = index.get_or_insert_with(target.clone(), |index| {
                    // Recursively strip debug info from the target routine.
                    let UninstalledRoutine { ref routine, id } = target;
                    UninstalledRoutine {
                        routine: Arc::new(Routine::clone(&*routine).strip_debug_info(index)),
                        id,
                    }
                });
                Action::Jump(converted)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use assert_matches::assert_matches;
    use ip_test_macro::ip_test;
    use net_types::ip::{Ip, Ipv4, Ipv6};
    use test_case::test_case;

    use super::*;
    use crate::{context::testutil::FakeDeviceClass, InterfaceMatcher, PacketMatcher};

    #[derive(Debug, Clone, PartialEq)]
    enum RuleId {
        Valid,
        Invalid,
    }

    fn rule<I: IpExt>(
        matcher: PacketMatcher<I, FakeDeviceClass>,
        validation_info: RuleId,
    ) -> Rule<I, FakeDeviceClass, RuleId> {
        Rule { matcher, action: Action::Drop, validation_info }
    }

    fn hook_with_rules<I: IpExt>(
        rules: Vec<Rule<I, FakeDeviceClass, RuleId>>,
    ) -> Hook<I, FakeDeviceClass, RuleId> {
        Hook { routines: vec![Routine { rules }] }
    }

    #[ip_test]
    #[test_case(
        hook_with_rules(vec![rule(
            PacketMatcher {
                in_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Ethernet)),
                ..Default::default()
            },
            RuleId::Valid,
        )]),
        UnavailableMatcher::OutInterface =>
        Ok(());
        "match on input interface in root routine when available"
    )]
    #[test_case(
        hook_with_rules(vec![rule(
            PacketMatcher {
                out_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Ethernet)),
                ..Default::default()
            },
            RuleId::Valid,
        )]),
        UnavailableMatcher::InInterface =>
        Ok(());
        "match on output interface in root routine when available"
    )]
    #[test_case(
        hook_with_rules(vec![
            rule(PacketMatcher::default(), RuleId::Valid),
            rule(
                PacketMatcher {
                    in_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Ethernet)),
                    ..Default::default()
                },
                RuleId::Invalid,
            ),
        ]),
        UnavailableMatcher::InInterface =>
        Err(ValidationError::RuleWithInvalidMatcher(RuleId::Invalid));
        "match on input interface in root routine when unavailable"
    )]
    #[test_case(
        hook_with_rules(vec![
            rule(PacketMatcher::default(), RuleId::Valid),
            rule(
                PacketMatcher {
                    out_interface: Some(InterfaceMatcher::DeviceClass(FakeDeviceClass::Ethernet)),
                    ..Default::default()
                },
                RuleId::Invalid,
            ),
        ]),
        UnavailableMatcher::OutInterface =>
        Err(ValidationError::RuleWithInvalidMatcher(RuleId::Invalid));
        "match on output interface in root routine when unavailable"
    )]
    #[test_case(
        Hook {
            routines: vec![Routine {
                rules: vec![Rule {
                    matcher: PacketMatcher::default(),
                    action: Action::Jump(UninstalledRoutine::new(
                        vec![rule(
                            PacketMatcher {
                                in_interface: Some(InterfaceMatcher::DeviceClass(
                                    FakeDeviceClass::Ethernet,
                                )),
                                ..Default::default()
                            },
                            RuleId::Invalid,
                        )],
                        0,
                    )),
                    validation_info: RuleId::Valid,
                }],
            }],
        },
        UnavailableMatcher::InInterface =>
        Err(ValidationError::RuleWithInvalidMatcher(RuleId::Invalid));
        "match on input interface in target routine when unavailable"
    )]
    #[test_case(
        Hook {
            routines: vec![Routine {
                rules: vec![Rule {
                    matcher: PacketMatcher::default(),
                    action: Action::Jump(UninstalledRoutine::new(
                        vec![rule(
                            PacketMatcher {
                                out_interface: Some(InterfaceMatcher::DeviceClass(
                                    FakeDeviceClass::Ethernet,
                                )),
                                ..Default::default()
                            },
                            RuleId::Invalid,
                        )],
                        0,
                    )),
                    validation_info: RuleId::Valid,
                }],
            }],
        },
        UnavailableMatcher::OutInterface =>
        Err(ValidationError::RuleWithInvalidMatcher(RuleId::Invalid));
        "match on output interface in target routine when unavailable"
    )]
    fn validate_interface_matcher_available<I: Ip + IpExt>(
        hook: Hook<I, FakeDeviceClass, RuleId>,
        unavailable_matcher: UnavailableMatcher,
    ) -> Result<(), ValidationError<RuleId>> {
        validate_hook(&hook, unavailable_matcher)
    }

    #[test]
    fn strip_debug_info_reuses_uninstalled_routines() {
        // Two routines in the hook jump to the same uninstalled routine.
        let uninstalled_routine =
            UninstalledRoutine::<Ipv4, FakeDeviceClass, _>::new(Vec::new(), 0);
        let hook = Hook {
            routines: vec![
                Routine {
                    rules: vec![Rule {
                        matcher: PacketMatcher::default(),
                        action: Action::Jump(uninstalled_routine.clone()),
                        validation_info: "rule-1",
                    }],
                },
                Routine {
                    rules: vec![Rule {
                        matcher: PacketMatcher::default(),
                        action: Action::Jump(uninstalled_routine),
                        validation_info: "rule-2",
                    }],
                },
            ],
        };

        // When we strip the debug info from the routines in the hook, all
        // jump targets should be converted 1:1. In this case, there are two
        // jump actions that refer to the same uninstalled routine, so that
        // uninstalled routine should be converted once, and the resulting jump
        // actions should both point to the same new uninstalled routine.
        let Hook { routines } = hook.strip_debug_info(&mut UninstalledRoutineIndex::default());
        let (first, second) = assert_matches!(
            &routines[..],
            [Routine { rules: first }, Routine { rules: second }] => (first, second)
        );
        let first = assert_matches!(
            &first[..],
            [Rule { action: Action::Jump(target), .. }] => target
        );
        let second = assert_matches!(
            &second[..],
            [Rule { action: Action::Jump(target), .. }] => target
        );
        assert_eq!(first, second);
    }
}
