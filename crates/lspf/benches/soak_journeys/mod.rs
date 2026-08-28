mod harness;
mod journeys;
mod model;
mod protocol;

use std::error::Error;
use std::fs;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use harness::{Recorder, ResourceCounts};
use model::{Arguments, Scenario, ScenarioMeasurement, ScenarioResult, WorkloadManifest};

pub type SoakResult<T> = Result<T, Box<dyn Error>>;

pub async fn run() -> SoakResult<()> {
    let raw_arguments: Vec<_> = std::env::args().skip(1).collect();
    if raw_arguments.is_empty()
        || raw_arguments
            .iter()
            .any(|argument| argument.starts_with("--test-threads"))
    {
        return Ok(());
    }

    let arguments = Arguments::parse()?;
    let workload: WorkloadManifest = serde_json::from_slice(&fs::read(&arguments.workloads)?)?;
    workload.validate()?;
    let counts = Arc::new(ResourceCounts::default());
    counts.install_tracing()?;
    let mut recorder = Recorder::new(arguments.time_series.clone(), workload.sample_interval())?;
    let run_started = Instant::now();
    let mut measurements = Vec::with_capacity(workload.scenarios.len());

    for scenario in workload.scenarios.iter().copied() {
        match journeys::run(scenario, &workload, &counts, &mut recorder).await {
            Ok(measurement) => measurements.push(measurement),
            Err(error) => {
                measurements.push(failed_measurement(scenario, counts.snapshot()));
                write_results(&arguments, &workload, run_started, &recorder, &measurements)?;
                return Err(format!("{scenario} scenario failed: {error}").into());
            }
        }
        write_results(&arguments, &workload, run_started, &recorder, &measurements)?;
    }
    Ok(())
}

fn failed_measurement(scenario: Scenario, resources: harness::Resources) -> ScenarioMeasurement {
    ScenarioMeasurement {
        result: ScenarioResult {
            name: scenario,
            result: "failure",
            terminal_outcome: "error",
            duration_milliseconds: 0,
            operations: 0,
            bytes: 0,
            terminal_resources: resources,
        },
        memory_growth_mib: 0.0,
    }
}

fn write_results(
    arguments: &Arguments,
    workload: &WorkloadManifest,
    run_started: Instant,
    recorder: &Recorder,
    measurements: &[ScenarioMeasurement],
) -> SoakResult<()> {
    let operations = measurements
        .iter()
        .map(|item| item.result.operations)
        .sum::<u64>();
    let bytes = measurements
        .iter()
        .map(|item| item.result.bytes)
        .sum::<u64>();
    let unexplained_growth_mib = measurements
        .iter()
        .map(|item| item.memory_growth_mib)
        .fold(0.0, f64::max);
    let scenarios: Vec<_> = measurements.iter().map(|item| &item.result).collect();
    let results = json!({
        "schemaVersion": 1,
        "workloadVersion": workload.workload_version,
        "revision": arguments.revision,
        "durationSeconds": run_started.elapsed().as_secs_f64(),
        "traffic": {"operations": operations, "bytes": bytes},
        "peakRssMiB": recorder.peak_rss_mib,
        "unexplainedGrowthMiB": unexplained_growth_mib,
        "scenarios": scenarios,
    });
    fs::write(&arguments.output, serde_json::to_vec_pretty(&results)?)?;
    Ok(())
}
