// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error};
use at_commands as at;

use super::{at_cmd, at_ok, at_resp};
use super::{Procedure, ProcedureInput, ProcedureOutput};

use crate::features::{extract_features_from_command, AgFeatures, CVSD, MSBC};
use crate::peer::indicators::{BATTERY_LEVEL, ENHANCED_SAFETY, INDICATOR_REPORTING_MODE};
use crate::peer::procedure_manipulated_state::ProcedureManipulatedState;

#[derive(Debug, PartialEq)]
pub enum State {
    SupportedFeaturesExchange, // Sent AT+BRSF, waiting for +BRSF.
    WaitingForOkAfterSupportedFeaturesExchange, // Received +BRSF, waiting for OK.
    CodecNegotiation,          // Sent AT+BAC, waiting for OK.
    TestAgIndicators,          // Sent AT+CIND=?, waiting for first +CIND
    WaitingForOkAfterTestAgIndicators, // Waiting for OK after +CIND
    ReadAgIndicators,          // Sent AT+CIND?, waiting for second +CIND
    WaitingForOkAfterReadAgIndicators, // Waiting for OL after second +CIND
    AgIndicatorStatusUpdate,   // Sent AT+CMER, waiting for OK.
    CallHoldAndMultiParty,     // Sent AT+CHLD, waiting for +CHLD
    WaitingForOkAfterCallHoldAndMultiParty, // Waiting for OK after +CHLD
    HfSupportedHfIndicators,   // Sent AT+BIND, waiting for OK.
    TestHfIndicators,          // Sent AT+BIND=?, waiting for +BIND.
    WaitingForOkAfterTestHfIndicators, // Waiting for OK after +BIND.
    ReadHfIndicators,          // Sent AT+BIND?, waiting for +BIND or OK.
    Terminated,
}

#[derive(Debug)]
pub struct SlcInitProcedure {
    state: State,
}

impl SlcInitProcedure {
    pub fn new() -> Self {
        Self { state: State::SupportedFeaturesExchange }
    }

    #[cfg(test)]
    pub fn start_at_state(state: State) -> Self {
        Self { state, ..SlcInitProcedure::new() }
    }

    #[cfg(test)]
    pub fn start_terminated() -> Self {
        Self { state: State::Terminated }
    }
}

impl Procedure for SlcInitProcedure {
    fn name(&self) -> &str {
        "SLC Initialization"
    }

    /// Checks for sequential ordering of commands by first checking the
    /// stage the SLCI is in and then extract important data from AG responses
    /// and proceed to next stage if necessary.
    fn transition(
        &mut self,
        state: &mut ProcedureManipulatedState,
        input: ProcedureInput,
    ) -> Result<Vec<ProcedureOutput>, Error> {
        if self.is_terminated() {
            return Err(format_err!(
                "Procedure is already terminated before processing update for response(s): {:?}.",
                input
            ));
        }

        let output_option = match (&self.state, input) {
            // Sent AT+BRSF, waiting for +BRSF /////////////////////////////////////////////////////
            (State::SupportedFeaturesExchange, at_resp!(Brsf { features })) => {
                state.ag_features = AgFeatures::from_bits_truncate(features);
                self.state = State::WaitingForOkAfterSupportedFeaturesExchange;
                None
            }

            // Received +BSRF, waiting for OK //////////////////////////////////////////////////////
            (State::WaitingForOkAfterSupportedFeaturesExchange, at_ok!()) => {
                if state.supports_codec_negotiation() {
                    self.state = State::CodecNegotiation;
                    state.supported_codecs.push(MSBC);
                    // TODO(fxb/130963) Make this configurable.
                    // By default, we support the CVSD and MSBC codecs.
                    Some(at_cmd!(Bac { codecs: vec![CVSD.into(), MSBC.into()] }))
                } else {
                    self.state = State::TestAgIndicators;
                    Some(at_cmd!(CindTest {}))
                }
            }

            // Sent AT+BAC, waiting for OK /////////////////////////////////////////////////////////
            (State::CodecNegotiation, at_ok!()) => {
                self.state = State::TestAgIndicators;
                Some(at_cmd!(CindTest {}))
            }

            // Sent AT+CIND=?, waiting for +CIND: //////////////////////////////////////////////////
            (
                State::TestAgIndicators,
                ProcedureInput::AtResponseFromAg(at::Response::RawBytes(_bytes)),
            ) => {
                // TODO(fxbug.dev/108331): Read additional indicators by parsing raw bytes instead
                // ofjust checking for existence of raw bytes.
                self.state = State::WaitingForOkAfterTestAgIndicators;
                None
            }

            // Received +CIND, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterTestAgIndicators, at_ok!()) => {
                self.state = State::ReadAgIndicators;
                Some(at_cmd!(CindRead {}))
            }

            // Sent AT+CIND?, waiting for +CIND: ///////////////////////////////////////////////////
            (
                State::ReadAgIndicators,
                ProcedureInput::AtResponseFromAg(at::Response::Success(
                    cmd @ at::Success::Cind { .. },
                )),
            ) => {
                state.ag_indicators.update_indicator_values(&cmd);
                self.state = State::WaitingForOkAfterReadAgIndicators;
                None
            }

            // Received +CIND:, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterReadAgIndicators, at_ok!()) => {
                self.state = State::AgIndicatorStatusUpdate;
                Some(at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 }))
            }

            // Sent AT+CMER, waiting for OK ////////////////////////////////////////////////////////
            (State::AgIndicatorStatusUpdate, at_ok!()) => {
                if state.supports_three_way_calling() {
                    self.state = State::CallHoldAndMultiParty;
                    Some(at_cmd!(ChldTest {}))
                } else if state.supports_hf_indicators() {
                    self.state = State::HfSupportedHfIndicators;
                    Some(at_cmd!(Bind {
                        indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64],
                    }))
                } else {
                    self.state = State::Terminated;
                    None
                }
            }

            // Sent AT+CHLD=?, waiting for +CHLD: //////////////////////////////////////////////////
            (State::CallHoldAndMultiParty, at_resp!(Chld { commands })) => {
                state.three_way_features = extract_features_from_command(&commands)?;
                self.state = State::WaitingForOkAfterCallHoldAndMultiParty;
                None
            }

            // Received +CHLD:, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterCallHoldAndMultiParty, at_ok!()) => {
                if state.supports_hf_indicators() {
                    self.state = State::HfSupportedHfIndicators;
                    Some(at_cmd!(Bind {
                        indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64]
                    }))
                } else {
                    self.state = State::Terminated;
                    None
                }
            }
            // Sent AT+BIND=, waiting for OK ///////////////////////////////////////////////////////
            (State::HfSupportedHfIndicators, at_ok!()) => {
                self.state = State::TestHfIndicators;
                Some(at_cmd!(BindTest {}))
            }

            // Sent AT+BIND=?, waiting for +BIND: ///////////////////////////////////////////////////
            (State::TestHfIndicators, at_resp!(BindList { indicators })) => {
                state.hf_indicators.set_supported_indicators(&indicators);
                self.state = State::WaitingForOkAfterTestHfIndicators;
                None
            }

            // Received +BIND:, waiting for OK /////////////////////////////////////////////////////
            (State::WaitingForOkAfterTestHfIndicators, at_ok!()) => {
                self.state = State::ReadHfIndicators;
                Some(at_cmd!(BindRead {}))
            }

            // Sent AT+BIND?, waiting for +BIND: or OK /////////////////////////////////////////////
            (
                State::ReadHfIndicators,
                ProcedureInput::AtResponseFromAg(at::Response::Success(
                    cmd @ at::Success::BindStatus { .. },
                )),
            ) => {
                state.hf_indicators.change_indicator_state(&cmd)?;
                None
            }

            // Sent AT+BIND?, waiting for +BIND: or OK /////////////////////////////////////////////
            (State::ReadHfIndicators, at_ok!()) => {
                self.state = State::Terminated;
                None
            }

            // Unexpected AT response for state ////////////////////////////////////////////////////
            (state, input) => {
                // Early return for error
                return Err(format_err!(
                    "Wrong responses at {:?} stage of SLCI with response(s): {:?}.",
                    state,
                    input
                ));
            }
        };

        let outputs = output_option.into_iter().collect();
        Ok(outputs)
    }

    fn is_terminated(&self) -> bool {
        self.state == State::Terminated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{config::HandsFreeFeatureSupport, features::CallHoldAction, features::HfFeatures};
    use assert_matches::assert_matches;

    // TODO(fxb/71668) Stop using raw bytes.
    const CIND_TEST_RESPONSE_BYTES: &[u8] = b"+CIND: \
    (\"service\",(0,1)),\
    (\"call\",(0,1)),\
    (\"callsetup\",(0,3)),\
    (\"callheld\",(0,2)),\
    (\"signal\",(0,5)),\
    (\"roam\",(0,1)),\
    (\"battchg\",(0,5)\
    )";

    #[fuchsia::test]
    /// Checks that the mandatory exchanges between the AG and HF roles properly progresses
    /// our state and sends the expected responses until our procedure it marked complete.
    fn slci_mandatory_exchanges_and_termination() {
        let mut procedure = SlcInitProcedure::new();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: AgFeatures::default().bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = ProcedureInput::AtResponseFromAg(at::Response::RawBytes(indicator_msg));
        let response2_ok = at_ok!();
        let expected_command2 = vec![at_cmd!(CindRead {})];
        assert_eq!(procedure.transition(&mut state, response2).unwrap(), vec![]);

        assert_eq!(procedure.transition(&mut state, response2_ok).unwrap(), expected_command2);

        let response3 = at_resp!(Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        });
        let response3_ok = at_ok!();
        let update3 =
            vec![at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 })];
        assert_eq!(procedure.transition(&mut state, response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response3_ok).unwrap(), update3);

        let response4 = at_ok!();
        assert_eq!(procedure.transition(&mut state, response4).unwrap(), vec![]);

        assert!(procedure.is_terminated());
    }

    #[fuchsia::test]
    fn slci_hf_indicator_properly_works() {
        let mut procedure = SlcInitProcedure::new();
        // Hf indicators needed for optional procedure.
        let mut hf_features = HfFeatures::default();
        let mut ag_features = AgFeatures::default();
        hf_features.set(HfFeatures::HF_INDICATORS, true);
        ag_features.set(AgFeatures::HF_INDICATORS, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!state.hf_indicators.enhanced_safety.1);
        assert!(!state.hf_indicators.battery_level.1);
        assert!(!state.hf_indicators.enhanced_safety.0.enabled);
        assert!(!state.hf_indicators.battery_level.0.enabled);
        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: ag_features.bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = ProcedureInput::AtResponseFromAg(at::Response::RawBytes(indicator_msg));
        let response2_ok = at_ok!();
        let expected_command2 = vec![at_cmd!(CindRead {})];

        assert_eq!(procedure.transition(&mut state, response2).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response2_ok).unwrap(), expected_command2);

        let response3 = at_resp!(Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        });
        let response3_ok = at_ok!();
        let expected_command3 =
            vec![at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 })];

        assert_eq!(procedure.transition(&mut state, response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response3_ok).unwrap(), expected_command3);

        let response4 = at_ok!();
        let expected_command4 =
            vec![at_cmd!(Bind { indicators: vec![ENHANCED_SAFETY as i64, BATTERY_LEVEL as i64] })];
        assert_eq!(procedure.transition(&mut state, response4).unwrap(), expected_command4);

        let response5 = at_ok!();
        let expected_command5 = vec![at_cmd!(BindTest {})];
        assert_eq!(procedure.transition(&mut state, response5).unwrap(), expected_command5);

        let response6 = at_resp!(BindList {
            indicators: vec![
                at::BluetoothHFIndicator::BatteryLevel,
                at::BluetoothHFIndicator::EnhancedSafety,
            ],
        });
        let response6_ok = at_ok!();
        let expected_command6 = vec![at_cmd!(BindRead {})];
        assert_eq!(procedure.transition(&mut state, response6).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response6_ok).unwrap(), expected_command6);
        assert!(state.hf_indicators.enhanced_safety.1);
        assert!(state.hf_indicators.battery_level.1);

        let response7 =
            at_resp!(BindStatus { anum: at::BluetoothHFIndicator::EnhancedSafety, state: true });
        let response8 =
            at_resp!(BindStatus { anum: at::BluetoothHFIndicator::BatteryLevel, state: true });
        let response9 = at_ok!();
        assert_eq!(procedure.transition(&mut state, response7).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response8).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response9).unwrap(), vec![]);
        assert!(state.hf_indicators.enhanced_safety.0.enabled);
        assert!(state.hf_indicators.battery_level.0.enabled);
    }

    #[fuchsia::test]
    fn slci_codec_negotiation_properly_works() {
        let mut procedure = SlcInitProcedure::new();
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::CODEC_NEGOTIATION, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::CODEC_NEGOTIATION, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: ag_features.bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(Bac { codecs: vec![CVSD.into(), MSBC.into()] })];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let response2 = at_ok!();
        let expected_command2 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response2).unwrap(), expected_command2);
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn slci_three_way_feature_proper_works() {
        let mut procedure = SlcInitProcedure::new();
        // Three way calling needed for stage progression.
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::THREE_WAY_CALLING, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::THREE_WAY_CALLING, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let response1 = at_resp!(Brsf { features: ag_features.bits() });
        let response1_ok = at_ok!();
        let expected_command1 = vec![at_cmd!(CindTest {})];

        assert_eq!(procedure.transition(&mut state, response1).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response1_ok).unwrap(), expected_command1);

        let indicator_msg = CIND_TEST_RESPONSE_BYTES.to_vec();
        let response2 = ProcedureInput::AtResponseFromAg(at::Response::RawBytes(indicator_msg));
        let response2_ok = at_ok!();
        let expected_command2 = vec![at_cmd!(CindRead {})];
        assert_eq!(procedure.transition(&mut state, response2).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response2_ok).unwrap(), expected_command2);

        let response3 = at_resp!(Cind {
            service: false,
            call: false,
            callsetup: 0,
            callheld: 0,
            signal: 0,
            roam: false,
            battchg: 0,
        });
        let response3_ok = at_ok!();
        let update3 =
            vec![at_cmd!(Cmer { mode: INDICATOR_REPORTING_MODE, keyp: 0, disp: 0, ind: 1 })];
        assert_eq!(procedure.transition(&mut state, response3).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response3_ok).unwrap(), update3);

        let response4 = at_ok!();
        let update4 = vec![at_cmd!(ChldTest {})];
        assert_eq!(procedure.transition(&mut state, response4).unwrap(), update4);

        let commands = vec![
            String::from("0"),
            String::from("1"),
            String::from("2"),
            String::from("11"),
            String::from("22"),
            String::from("3"),
            String::from("4"),
        ];
        let response5 = at_resp!(Chld { commands });
        let response5_ok = at_ok!();
        assert_eq!(procedure.transition(&mut state, response5).unwrap(), vec![]);
        assert_eq!(procedure.transition(&mut state, response5_ok).unwrap(), vec![]);
        assert!(procedure.is_terminated());

        let features = vec![
            CallHoldAction::ReleaseAllHeld,
            CallHoldAction::ReleaseAllActive,
            CallHoldAction::HoldActiveAndAccept,
            CallHoldAction::ReleaseSpecified(1),
            CallHoldAction::HoldAllExceptSpecified(2),
            CallHoldAction::AddCallToHeldConversation,
            CallHoldAction::ExplicitCallTransfer,
        ];

        assert_eq!(features, state.three_way_features);
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_feature_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::SupportedFeaturesExchange);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_codec_negotiation_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::CodecNegotiation);
        let mut hf_features = HfFeatures::default();
        hf_features.set(HfFeatures::CODEC_NEGOTIATION, true);
        let mut ag_features = AgFeatures::default();
        ag_features.set(AgFeatures::CODEC_NEGOTIATION, true);
        let mut state = ProcedureManipulatedState::load_with_set_features(hf_features, ag_features);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_list_indicators_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::TestAgIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_enable_indicators_stage() {
        let mut procedure =
            SlcInitProcedure::start_at_state(State::WaitingForOkAfterReadAgIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_indicator_update_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::AgIndicatorStatusUpdate);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_call_hold_stage_non_number_index() {
        let mut procedure = SlcInitProcedure::start_at_state(State::CallHoldAndMultiParty);
        let config = HandsFreeFeatureSupport {
            call_waiting_or_three_way_calling: true,
            ..HandsFreeFeatureSupport::default()
        };
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let invalid_command = vec![String::from("1A")];
        let response = at_resp!(Chld { commands: invalid_command });

        assert_matches!(procedure.transition(&mut state, response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_call_hold_stage_invalid_command() {
        let mut procedure = SlcInitProcedure::start_at_state(State::CallHoldAndMultiParty);
        let config = HandsFreeFeatureSupport {
            call_waiting_or_three_way_calling: true,
            ..HandsFreeFeatureSupport::default()
        };
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let invalid_command = vec![String::from("5")];
        let response = at_resp!(Chld { commands: invalid_command });

        assert_matches!(procedure.transition(&mut state, response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::HfSupportedHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_request_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::TestHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_incorrect_response_at_hf_indicator_enable_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::ReadHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        // Did not receive expected Ok response as should result in error.
        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_no_ok_at_hf_indicator_enable_stage() {
        let mut procedure = SlcInitProcedure::start_at_state(State::ReadHfIndicators);
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(!procedure.is_terminated());

        let wrong_response = at_resp!(TestResponse {});
        assert_matches!(procedure.transition(&mut state, wrong_response), Err(_));
        assert!(!procedure.is_terminated());
    }

    #[fuchsia::test]
    fn error_when_update_on_terminated_procedure() {
        let mut procedure = SlcInitProcedure::start_terminated();
        let config = HandsFreeFeatureSupport::default();
        let mut state = ProcedureManipulatedState::new(config);

        assert!(procedure.is_terminated());
        // Valid response of first step of SLCI
        let valid_response = at_resp!(Brsf { features: AgFeatures::default().bits() });
        let update = procedure.transition(&mut state, valid_response);
        assert_matches!(update, Err(_));
    }
}
