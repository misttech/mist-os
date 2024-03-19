// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::sync::Arc;
use core::marker::PhantomData;

use derivative::Derivative;
use once_cell::sync::OnceCell;
use packet_formats::ip::IpExt;

use crate::{
    state::UninstalledRoutineInner, Action, Hook, IpRoutines, NatRoutines, Routine, Rule, State,
    UninstalledRoutine,
};

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
pub struct ValidState<I: IpExt, DeviceClass>(State<I, DeviceClass, ()>);

impl<I: IpExt, DeviceClass> ValidState<I, DeviceClass> {
    /// Accesses the inner state.
    pub fn get(&self) -> &State<I, DeviceClass, ()> {
        let Self(state) = self;
        &state
    }
}

/// Ancillary information that is stored alongside filtering state that is in
/// the process of being validated and installed.
///
/// Bindings defines this type for rules, where it is an identifying tag
/// attached to each rule that allows Core to report which rule caused a
/// particular error. For uninstalled routines, this is a pointer to the
/// converted version of the routine that has had its debug info stripped.
pub struct ValidationInfo<RuleInfo> {
    _marker: PhantomData<RuleInfo>,
}

impl<RuleInfo> crate::state::ValidationInfo for ValidationInfo<RuleInfo> {
    type UninstalledRoutine<I: IpExt, DeviceClass> =
        OnceCell<Arc<UninstalledRoutineInner<I, DeviceClass, ()>>>;
    type Rule = RuleInfo;
}

impl<I: IpExt, DeviceClass: Clone> ValidState<I, DeviceClass> {
    /// Validates the provide state and creates a new `ValidState` or returns a
    /// `ValidationError` if the state is invalid.
    ///
    /// The provided state must not contain any cyclical routine graphs (formed by
    /// rules with jump actions). The behavior in this case is unspecified but could
    /// be a deadlock or a panic, for example.
    ///
    /// TODO(https://fxbug.dev/325492760): replace usage of
    /// [`once_cell::sync::OnceCell`] with `std::sync::OnceLock`, which always
    /// panics when called reentrantly.
    pub fn new<RuleInfo: Clone>(
        state: State<I, DeviceClass, ValidationInfo<RuleInfo>>,
    ) -> Result<Self, ValidationError<RuleInfo>> {
        let State { ip_routines, nat_routines } = &state;

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

        Ok(Self(state.strip_debug_info()))
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
    Hook { routines }: &Hook<I, DeviceClass, ValidationInfo<RuleInfo>>,
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
    Routine { rules }: &Routine<I, DeviceClass, ValidationInfo<RuleInfo>>,
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
                let UninstalledRoutine(inner) = target;
                let UninstalledRoutineInner { routine, validation_info: _ } = &**inner;
                validate_routine(&routine, unavailable_matcher)?
            }
        }
    }

    Ok(())
}

impl<I: IpExt, DeviceClass: Clone, RuleInfo: Clone>
    State<I, DeviceClass, ValidationInfo<RuleInfo>>
{
    fn strip_debug_info(self) -> State<I, DeviceClass, ()> {
        let Self { ip_routines, nat_routines } = self;
        State {
            ip_routines: ip_routines.strip_debug_info(),
            nat_routines: nat_routines.strip_debug_info(),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone, RuleInfo: Clone>
    IpRoutines<I, DeviceClass, ValidationInfo<RuleInfo>>
{
    fn strip_debug_info(self) -> IpRoutines<I, DeviceClass, ()> {
        let Self { ingress, local_ingress, egress, local_egress, forwarding } = self;
        IpRoutines {
            ingress: ingress.strip_debug_info(),
            local_ingress: local_ingress.strip_debug_info(),
            forwarding: forwarding.strip_debug_info(),
            egress: egress.strip_debug_info(),
            local_egress: local_egress.strip_debug_info(),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone, RuleInfo: Clone>
    NatRoutines<I, DeviceClass, ValidationInfo<RuleInfo>>
{
    fn strip_debug_info(self) -> NatRoutines<I, DeviceClass, ()> {
        let Self { ingress, local_ingress, egress, local_egress } = self;
        NatRoutines {
            ingress: ingress.strip_debug_info(),
            local_ingress: local_ingress.strip_debug_info(),
            egress: egress.strip_debug_info(),
            local_egress: local_egress.strip_debug_info(),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone, RuleInfo: Clone> Hook<I, DeviceClass, ValidationInfo<RuleInfo>> {
    fn strip_debug_info(self) -> Hook<I, DeviceClass, ()> {
        let Self { routines } = self;
        Hook { routines: routines.into_iter().map(|routine| routine.strip_debug_info()).collect() }
    }
}

impl<I: IpExt, DeviceClass: Clone, RuleInfo: Clone>
    Routine<I, DeviceClass, ValidationInfo<RuleInfo>>
{
    fn strip_debug_info(self) -> Routine<I, DeviceClass, ()> {
        let Self { rules } = self;
        Routine {
            rules: rules
                .into_iter()
                .map(|Rule { matcher, action, validation_info: _ }| Rule {
                    matcher,
                    action: action.strip_debug_info(),
                    validation_info: (),
                })
                .collect(),
        }
    }
}

impl<I: IpExt, DeviceClass: Clone, RuleInfo: Clone>
    Action<I, DeviceClass, ValidationInfo<RuleInfo>>
{
    fn strip_debug_info(self) -> Action<I, DeviceClass, ()> {
        match self {
            Self::Accept => Action::Accept,
            Self::Drop => Action::Drop,
            Self::Return => Action::Return,
            Self::Jump(target) => {
                // NB: it is an error to call `OnceCell::get_or_init` reentrantly. The exact
                // behavior is currently left unspecified (it may deadlock or panic, for example).
                // Callers must take care to provide filtering state that does not contain cyclical
                // routines.
                //
                // TODO(https://fxbug.dev/325492760): replace this with `std::sync::OnceLock` which
                // always panics when called reentrantly.
                let UninstalledRoutine(target) = target;
                let UninstalledRoutineInner { routine, validation_info: converted_routine } =
                    &*target;
                let inner = converted_routine
                    .get_or_init(|| {
                        // Convert the target routine and store a pointer to it in the original
                        // routine's validation_info so that the next time we attempt to convert it,
                        // we just reuse the already-converted routine.
                        let converted = Routine::clone(&routine).strip_debug_info();
                        Arc::new(UninstalledRoutineInner {
                            routine: converted,
                            validation_info: (),
                        })
                    })
                    .clone();
                Action::Jump(UninstalledRoutine(inner))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

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
    ) -> Rule<I, FakeDeviceClass, ValidationInfo<RuleId>> {
        Rule { matcher, action: Action::Drop, validation_info }
    }

    fn hook_with_rules<I: IpExt>(
        rules: Vec<Rule<I, FakeDeviceClass, ValidationInfo<RuleId>>>,
    ) -> Hook<I, FakeDeviceClass, ValidationInfo<RuleId>> {
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
                    action: Action::Jump(UninstalledRoutine::new(vec![rule(
                        PacketMatcher {
                            in_interface: Some(InterfaceMatcher::DeviceClass(
                                FakeDeviceClass::Ethernet,
                            )),
                            ..Default::default()
                        },
                        RuleId::Invalid,
                    )])),
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
                    action: Action::Jump(UninstalledRoutine::new(vec![rule(
                        PacketMatcher {
                            out_interface: Some(InterfaceMatcher::DeviceClass(
                                FakeDeviceClass::Ethernet,
                            )),
                            ..Default::default()
                        },
                        RuleId::Invalid,
                    )])),
                    validation_info: RuleId::Valid,
                }],
            }],
        },
        UnavailableMatcher::OutInterface =>
        Err(ValidationError::RuleWithInvalidMatcher(RuleId::Invalid));
        "match on output interface in target routine when unavailable"
    )]
    fn validate_interface_matcher_available<I: Ip + IpExt>(
        hook: Hook<I, FakeDeviceClass, ValidationInfo<RuleId>>,
        unavailable_matcher: UnavailableMatcher,
    ) -> Result<(), ValidationError<RuleId>> {
        validate_hook(&hook, unavailable_matcher)
    }

    #[test]
    fn strip_debug_info_reuses_uninstalled_routines() {
        // Two routines in the hook jump to the same uninstalled routine.
        let uninstalled_routine = UninstalledRoutine::<Ipv4, FakeDeviceClass, _>::new(vec![]);
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
        let Hook { routines } = hook.strip_debug_info();
        let (first, second) = assert_matches!(
            &routines[..],
            [Routine { rules: first }, Routine { rules: second }] => (first, second)
        );
        let UninstalledRoutine(first) = assert_matches!(
            &first[..],
            [Rule { action: Action::Jump(target), .. }] => target
        );
        let UninstalledRoutine(second) = assert_matches!(
            &second[..],
            [Rule { action: Action::Jump(target), .. }] => target
        );
        assert!(Arc::ptr_eq(first, second));
    }
}
