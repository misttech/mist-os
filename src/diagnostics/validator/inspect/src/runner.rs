// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    super::{
        data::Data,
        puppet, results,
        trials::{self, Step},
        validate, PUPPET_MONIKER,
    },
    anyhow::{bail, Error},
    diagnostics_reader::{ArchiveReader, ComponentSelector, Inspect},
    fidl_diagnostics_validate::{
        TestFailure, TestResult, TestSuccess, ValidateResult,
        ValidateResultsIteratorGetNextResponse, ValidateResultsIteratorRequest,
        ValidateResultsIteratorRequestStream, ValidatorRequest, ValidatorRequestStream,
    },
    futures::StreamExt,
};

pub async fn serve_connection_requests(stream: ValidatorRequestStream) {
    stream
        .for_each_concurrent(None, |request| async move {
            let Ok(request) = request else {
                    return;
                };
            match request {
                ValidatorRequest::Validate { results, .. } => {
                    let stream = results.into_stream().expect("valid stream");
                    run_all_trials(stream).await;
                }
            }
        })
        .await;
}

async fn publish_puppet(
    puppet: &mut puppet::Puppet,
) -> Result<(), ValidateResultsIteratorGetNextResponse> {
    match puppet.publish().await {
        Ok(TestResult::Ok) => Ok(()),
        Ok(failed_result) => Err(ValidateResultsIteratorGetNextResponse {
            result: Some(ValidateResult::Failure(TestFailure {
                test_name: "Test archive".to_string(),
                reason: format!("Publish reported {failed_result:?}"),
            })),
            ..ValidateResultsIteratorGetNextResponse::default()
        }),
        Err(e) => Err(ValidateResultsIteratorGetNextResponse {
            result: Some(ValidateResult::Failure(TestFailure {
                test_name: "Test archive".to_string(),
                reason: format!("Publish error: {e:?}"),
            })),
            ..ValidateResultsIteratorGetNextResponse::default()
        }),
    }
}

async fn unpublish_puppet(
    puppet: &mut puppet::Puppet,
) -> Result<(), ValidateResultsIteratorGetNextResponse> {
    match puppet.unpublish().await {
        Ok(TestResult::Ok) => Ok(()),
        Ok(failed_result) => Err(ValidateResultsIteratorGetNextResponse {
            result: Some(ValidateResult::Failure(TestFailure {
                test_name: "Test archive".to_string(),
                reason: format!("Unpublish reported {failed_result:?}"),
            })),
            ..ValidateResultsIteratorGetNextResponse::default()
        }),
        Err(e) => Err(ValidateResultsIteratorGetNextResponse {
            result: Some(ValidateResult::Failure(TestFailure {
                test_name: "Test archive".to_string(),
                reason: format!("Unpublish error: {e:?}"),
            })),
            ..ValidateResultsIteratorGetNextResponse::default()
        }),
    }
}

pub async fn run_all_trials(mut stream: ValidateResultsIteratorRequestStream) {
    let mut results = results::Results::new();
    let mut trial_set = trials::real_trials();
    while let Some(Ok(request)) = stream.next().await {
        match request {
            ValidateResultsIteratorRequest::GetNext { responder } => {
                match puppet::Puppet::connect().await {
                    Ok(mut puppet) => {
                        results.diff_type = puppet.config.diff_type;
                        if puppet.config.test_archive {
                            if let Err(e) = publish_puppet(&mut puppet).await {
                                responder.send(e).ok();
                                continue;
                            }
                        }

                        let Some(mut trial) = trial_set.pop() else {
                            responder.send(ValidateResultsIteratorGetNextResponse::default()).ok();
                            continue;
                        };

                        let mut data = Data::new();
                        let trial_result =
                            run_trial(&mut puppet, &mut data, &mut trial, &mut results).await;

                        if puppet.config.test_archive {
                            if let Err(e) = unpublish_puppet(&mut puppet).await {
                                responder.send(e).ok();
                                continue;
                            }
                        }

                        match trial_result {
                            Err(failed) => responder.send(failed).ok(),
                            Ok(passed) => responder.send(passed).ok(),
                        };
                    }

                    Err(e) => {
                        responder
                            .send(ValidateResultsIteratorGetNextResponse {
                                result: Some(ValidateResult::Failure(TestFailure {
                                    test_name: "Failed to start up tests".to_string(),
                                    reason: format!(
                                        "Failed to form unknown Puppet - error {:?}.",
                                        e
                                    ),
                                })),
                                ..ValidateResultsIteratorGetNextResponse::default()
                            })
                            .ok();
                        continue;
                    }
                }
            }
        }
    }
}

#[derive(PartialEq)]
enum StepResult {
    // Signals that we should continue with the next step.
    Continue,
    // Signals that the state after executing this step is inconsistent, we should stop running
    // further steps.
    Stop,
}

fn into_response(trial_name: String, err: Error) -> ValidateResultsIteratorGetNextResponse {
    ValidateResultsIteratorGetNextResponse {
        result: Some(ValidateResult::Failure(TestFailure {
            test_name: trial_name,
            reason: format!("{err:?}"),
        })),
        ..ValidateResultsIteratorGetNextResponse::default()
    }
}

async fn run_trial(
    puppet: &mut puppet::Puppet,
    data: &mut Data,
    trial: &mut trials::Trial,
    results: &mut results::Results,
) -> Result<ValidateResultsIteratorGetNextResponse, ValidateResultsIteratorGetNextResponse> {
    let trial_name = format!("{}:{}", puppet.printable_name(), trial.name);
    // We have to give explicit type here because compiler can't deduce it from None option value.
    try_compare::<validate::Action>(data, puppet, &trial_name, -1, None, -1, results)
        .await
        .map_err(|e| into_response(trial_name.clone(), e))?;
    for (step_index, step) in trial.steps.iter_mut().enumerate() {
        let step_result = match step {
            Step::Actions(actions) => {
                run_actions(actions, data, puppet, &trial.name, step_index, results)
                    .await
                    .map_err(|e| into_response(trial_name.clone(), e))?
            }
            Step::WithMetrics(actions, step_name) => {
                let r = run_actions(actions, data, puppet, &trial.name, step_index, results)
                    .await
                    .map_err(|e| into_response(trial_name.clone(), e))?;
                results.remember_metrics(
                    puppet.metrics().map_err(|e| into_response(trial_name.clone(), e))?,
                    &trial.name,
                    step_index,
                    step_name,
                );
                r
            }
            Step::LazyActions(actions) => {
                run_lazy_actions(actions, data, puppet, &trial.name, step_index, results)
                    .await
                    .map_err(|e| into_response(trial_name.clone(), e))?
            }
        };
        if step_result == StepResult::Stop {
            break;
        }
    }
    Ok(ValidateResultsIteratorGetNextResponse {
        result: Some(ValidateResult::Success(TestSuccess { test_name: trial.name.clone() })),
        ..ValidateResultsIteratorGetNextResponse::default()
    })
}

async fn run_actions(
    actions: &mut Vec<validate::Action>,
    data: &mut Data,
    puppet: &mut puppet::Puppet,
    trial_name: &str,
    step_index: usize,
    results: &mut results::Results,
) -> Result<StepResult, Error> {
    for (action_number, action) in actions.iter_mut().enumerate() {
        if let Err(e) = data.apply(action) {
            bail!(
                "Local-apply error in trial {}, step {}, action {}: {:?} ",
                trial_name,
                step_index,
                action_number,
                e
            );
        }
        match puppet.apply(action).await {
            Err(e) => {
                bail!(
                    "Puppet-apply error in trial {}, step {}, action {}: {:?} ",
                    trial_name,
                    step_index,
                    action_number,
                    e
                );
            }
            Ok(validate::TestResult::Ok) => {}
            Ok(validate::TestResult::Unimplemented) => {
                results.unimplemented(puppet.printable_name(), action);
                return Ok(StepResult::Stop);
            }
            Ok(bad_result) => {
                bail!(
                    "In trial {}, puppet {} reported action {:?} was {:?}",
                    trial_name,
                    puppet.printable_name(),
                    action,
                    bad_result
                );
            }
        }
        try_compare(
            data,
            puppet,
            &trial_name,
            step_index as i32,
            Some(action),
            action_number as i32,
            results,
        )
        .await?;
    }
    Ok(StepResult::Continue)
}

async fn run_lazy_actions(
    actions: &mut Vec<validate::LazyAction>,
    data: &mut Data,
    puppet: &mut puppet::Puppet,
    trial_name: &str,
    step_index: usize,
    results: &mut results::Results,
) -> Result<StepResult, Error> {
    for (action_number, action) in actions.iter_mut().enumerate() {
        if let Err(e) = data.apply_lazy(action) {
            bail!(
                "Local-apply_lazy error in trial {}, step {}, action {}: {:?} ",
                trial_name,
                step_index,
                action_number,
                e
            );
        }
        match puppet.apply_lazy(action).await {
            Err(e) => {
                bail!(
                    "Puppet-apply_lazy error in trial {}, step {}, action {}: {:?} ",
                    trial_name,
                    step_index,
                    action_number,
                    e
                );
            }
            Ok(validate::TestResult::Ok) => {}
            Ok(validate::TestResult::Unimplemented) => {
                results.unimplemented(puppet.printable_name(), action);
                return Ok(StepResult::Stop);
            }
            Ok(bad_result) => {
                bail!(
                    "In trial {}, puppet {} reported action {:?} was {:?}",
                    trial_name,
                    puppet.printable_name(),
                    action,
                    bad_result
                );
            }
        }
        try_compare(
            data,
            puppet,
            &trial_name,
            step_index as i32,
            Some(action),
            action_number as i32,
            results,
        )
        .await?;
    }
    Ok(StepResult::Continue)
}

async fn try_compare<ActionType: std::fmt::Debug>(
    data: &mut Data,
    puppet: &puppet::Puppet,
    trial_name: &str,
    step_index: i32,
    action: Option<&ActionType>,
    action_number: i32,
    results: &results::Results,
) -> Result<(), Error> {
    if !data.is_empty() {
        match puppet.read_data().await {
            Err(e) => {
                bail!(
                    "Puppet-read error in trial {}, step {}, action {} {:?}: {:?} ",
                    trial_name,
                    step_index,
                    action_number,
                    action,
                    e
                );
            }
            Ok(mut puppet_data) => {
                if puppet.config.has_runner_node {
                    puppet_data.remove_tree("runner");
                }
                if let Err(e) = data.compare(&puppet_data, results.diff_type) {
                    bail!(
                        "Compare error in trial {}, step {}, action {}:\n{:?}:\n{} ",
                        trial_name,
                        step_index,
                        action_number,
                        action,
                        e
                    );
                }
            }
        }
        if results.test_archive {
            let archive_data = match ArchiveReader::new()
                .add_selector(ComponentSelector::new(vec![PUPPET_MONIKER.to_string()]))
                .add_selector(
                    ComponentSelector::new(vec![puppet.printable_name().to_string()])
                        .with_tree_selector("root:DUMMY"),
                )
                .snapshot::<Inspect>()
                .await
            {
                Ok(archive_data) => archive_data,
                Err(e) => {
                    bail!(
                        "Archive read error in trial {}, step {}, action {}:\n{:?}:\n{} ",
                        trial_name,
                        step_index,
                        action_number,
                        action,
                        e
                    );
                }
            };
            if archive_data.len() != 1 {
                bail!(
                    "Expected 1 component in trial {}, step {}, action {}:\n{:?}:\nfound {} ",
                    trial_name,
                    step_index,
                    action_number,
                    action,
                    archive_data.len()
                );
            }

            let mut hierarchy_data: Data = archive_data[0].payload.as_ref().unwrap().clone().into();
            if puppet.config.has_runner_node {
                hierarchy_data.remove_tree("runner");
            }
            if let Err(e) = data.compare_to_json(&hierarchy_data, results.diff_type) {
                bail!(
                    "Archive compare error in trial {}, step {}, action {}:\n{:?}:\n{} ",
                    trial_name,
                    step_index,
                    action_number,
                    action,
                    e
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::trials::tests::trial_with_action,
        crate::trials::{Step, Trial},
        crate::*,
        fidl_diagnostics_validate::*,
    };

    #[fuchsia::test]
    async fn unimplemented_works() {
        let mut int_maker = trial_with_action(
            "foo",
            create_numeric_property!(
            parent: ROOT_ID, id: 1, name: "int", value: Value::IntT(0)),
        );
        let mut uint_maker = trial_with_action(
            "foo",
            create_numeric_property!(
            parent: ROOT_ID, id: 2, name: "uint", value: Value::UintT(0)),
        );
        let mut uint_create_delete = Trial {
            name: "foo".to_string(),
            steps: vec![
                Step::Actions(vec![
                    create_numeric_property!(parent: ROOT_ID, id: 2, name: "uint", value: Value::UintT(0)),
                ]),
                Step::Actions(vec![delete_property!(id: 2)]),
            ],
        };
        let mut results = results::Results::new();
        let mut puppet = puppet::tests::local_incomplete_puppet().await.unwrap();
        // results contains a list of the _un_implemented actions. local_incomplete_puppet()
        // implements Int creation, but not Uint. So results should not include Uint but should
        // include Int.
        {
            let mut data = Data::new();
            run_trial(&mut puppet, &mut data, &mut int_maker, &mut results).await.unwrap();
        }
        {
            let mut data = Data::new();
            run_trial(&mut puppet, &mut data, &mut uint_maker, &mut results).await.unwrap();
        }
        {
            let mut data = Data::new();
            run_trial(&mut puppet, &mut data, &mut uint_create_delete, &mut results).await.unwrap();
        }
        assert!(!results
            .to_json()
            .contains(&format!("{}: CreateProperty(Int)", puppet.printable_name())));
        assert!(results
            .to_json()
            .contains(&format!("{}: CreateProperty(Uint)", puppet.printable_name())));
    }
}
