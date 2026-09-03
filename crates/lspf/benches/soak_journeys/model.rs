use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use lspf::ResourcePolicy;
use serde::{Deserialize, Serialize};

use super::SoakResult;
use crate::soak::harness::Resources;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    Request,
    Cancellation,
    Edit,
    Progress,
    SlowPeer,
    Reconnect,
    Shutdown,
}

impl Scenario {
    pub const ALL: [Self; 7] = [
        Self::Request,
        Self::Cancellation,
        Self::Edit,
        Self::Progress,
        Self::SlowPeer,
        Self::Reconnect,
        Self::Shutdown,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Cancellation => "cancellation",
            Self::Edit => "edit",
            Self::Progress => "progress",
            Self::SlowPeer => "slow-peer",
            Self::Reconnect => "reconnect",
            Self::Shutdown => "shutdown",
        }
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadManifest {
    pub schema_version: u64,
    pub workload_version: u64,
    pub duration_seconds: u64,
    pub sample_interval_milliseconds: u64,
    pub scenarios: Vec<Scenario>,
    pub traffic: Traffic,
    pub limits: Limits,
}

impl WorkloadManifest {
    pub fn duration(&self) -> Duration {
        Duration::from_secs(self.duration_seconds)
    }

    pub fn sample_interval(&self) -> Duration {
        Duration::from_millis(self.sample_interval_milliseconds)
    }

    pub fn validate(&self) -> SoakResult<()> {
        let traffic = &self.traffic;
        let limits = self.limits;
        if self.schema_version != 1
            || self.duration_seconds == 0
            || self.sample_interval_milliseconds == 0
            || self.scenarios.is_empty()
            || !self.scenarios.windows(2).all(|pair| {
                Scenario::ALL
                    .iter()
                    .position(|scenario| *scenario == pair[0])
                    < Scenario::ALL
                        .iter()
                        .position(|scenario| *scenario == pair[1])
            })
            || traffic.request_concurrency == 0
            || traffic.cancellation_concurrency == 0
            || traffic.edit_document_bytes == 0
            || traffic.progress_concurrency == 0
            || traffic.slow_peer_attempts_per_cycle == 0
            || traffic.reconnects_per_cycle == 0
            || traffic.shutdowns_per_cycle == 0
            || limits.inbound_requests < traffic.request_concurrency
            || limits.inbound_requests < traffic.cancellation_concurrency
            || limits.outbound_messages == 0
            || limits.outbound_bytes < 1024
            || limits.documents == 0
            || limits.document_bytes < traffic.edit_document_bytes
            || limits.handler_timeout_milliseconds == 0
            || limits.outbound_request_timeout_milliseconds == 0
        {
            return Err(
                "soak workload duration, traffic, and limits must be positive and valid".into(),
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Traffic {
    pub request_concurrency: usize,
    pub cancellation_concurrency: usize,
    pub edit_document_bytes: usize,
    pub progress_concurrency: usize,
    pub slow_peer_attempts_per_cycle: usize,
    pub reconnects_per_cycle: usize,
    pub shutdowns_per_cycle: usize,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub inbound_requests: usize,
    pub outbound_messages: usize,
    pub outbound_bytes: usize,
    pub documents: usize,
    pub document_bytes: usize,
    pub handler_timeout_milliseconds: u64,
    pub outbound_request_timeout_milliseconds: u64,
}

impl Limits {
    pub fn policy(self) -> ResourcePolicy {
        ResourcePolicy {
            max_inbound_requests: self.inbound_requests,
            max_outbound_messages: self.outbound_messages,
            max_outbound_bytes: self.outbound_bytes,
            max_documents: self.documents,
            max_document_bytes: self.document_bytes,
            max_notebooks: ResourcePolicy::default().max_notebooks,
            handler_timeout: Duration::from_millis(self.handler_timeout_milliseconds),
            outbound_request_timeout: Some(Duration::from_millis(
                self.outbound_request_timeout_milliseconds,
            )),
        }
    }
}

pub struct Arguments {
    pub workloads: PathBuf,
    pub output: PathBuf,
    pub time_series: PathBuf,
    pub revision: String,
}

impl Arguments {
    pub fn parse() -> SoakResult<Self> {
        let mut args = std::env::args().skip(1);
        let mut workloads = None;
        let mut output = None;
        let mut time_series = None;
        let mut revision = None;
        while let Some(argument) = args.next() {
            if argument == "--bench" {
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {argument}"))?;
            match argument.as_str() {
                "--workloads" => workloads = Some(PathBuf::from(value)),
                "--output" => output = Some(PathBuf::from(value)),
                "--timeseries" => time_series = Some(PathBuf::from(value)),
                "--revision" => revision = Some(value),
                _ => return Err(format!("unknown argument: {argument}").into()),
            }
        }
        Ok(Self {
            workloads: workloads.ok_or("missing --workloads")?,
            output: output.ok_or("missing --output")?,
            time_series: time_series.ok_or("missing --timeseries")?,
            revision: revision.ok_or("missing --revision")?,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioResult {
    pub name: Scenario,
    pub result: &'static str,
    pub terminal_outcome: &'static str,
    pub duration_milliseconds: u64,
    pub operations: u64,
    pub bytes: u64,
    pub terminal_resources: Resources,
}

pub struct ScenarioMeasurement {
    pub result: ScenarioResult,
    pub memory_growth_mib: f64,
}
