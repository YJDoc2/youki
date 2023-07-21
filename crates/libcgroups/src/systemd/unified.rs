use std::{collections::HashMap, num::ParseIntError};
use zbus::zvariant::Value;

use super::{
    controller::Controller,
    cpu::{self, convert_shares_to_cgroup2},
    cpuset::{self, to_bitmask, BitmaskError},
    memory, pids,
};
use crate::common::ControllerOpt;

#[derive(thiserror::Error, Debug)]
pub enum SystemdUnifiedError {
    #[error("failed to parse cpu weight {value}: {err}")]
    CpuWeight { err: ParseIntError, value: String },
    #[error("invalid format for cpu.max: {0}")]
    CpuMax(String),
    #[error("failed to to parse cpu quota {value}: {err}")]
    CpuQuota { err: ParseIntError, value: String },
    #[error("failed to to parse cpu period {value}: {err}")]
    CpuPeriod { err: ParseIntError, value: String },
    #[error("setting {0} requires systemd version greater than 243")]
    OldSystemd(String),
    #[error("invalid value for cpuset.cpus {0}")]
    CpuSetCpu(BitmaskError),
    #[error("failed to parse {name} {value}: {err}")]
    Memory {
        err: ParseIntError,
        name: String,
        value: String,
    },
    #[error("failed to to parse pids.max {value}: {err}")]
    PidsMax { err: ParseIntError, value: String },
}

pub struct Unified {}

impl Controller for Unified {
    type Error = SystemdUnifiedError;

    fn apply(
        options: &ControllerOpt,
        systemd_version: u32,
        properties: &mut HashMap<&str, Value>,
    ) -> Result<(), Self::Error> {
        if let Some(unified) = options.resources.unified() {
            tracing::debug!("applying unified resource restrictions");
            Self::apply(unified, systemd_version, properties)?;
        }

        Ok(())
    }
}

impl Unified {
    fn apply(
        unified: &HashMap<String, String>,
        systemd_version: u32,
        properties: &mut HashMap<&str, Value>,
    ) -> Result<(), SystemdUnifiedError> {
        for (key, value) in unified {
            match key.as_str() {
                "cpu.weight" => {
                    let shares =
                        value
                            .parse::<u64>()
                            .map_err(|err| SystemdUnifiedError::CpuWeight {
                                err,
                                value: value.into(),
                            })?;
                    properties.insert(
                        cpu::CPU_WEIGHT,
                        Value::U64(convert_shares_to_cgroup2(shares)),
                    );
                }
                "cpu.max" => {
                    let parts: Vec<&str> = value.split_whitespace().collect();
                    if parts.is_empty() || parts.len() > 2 {
                        return Err(SystemdUnifiedError::CpuMax(value.into()));
                    }

                    let quota =
                        parts[0]
                            .parse::<u64>()
                            .map_err(|err| SystemdUnifiedError::CpuQuota {
                                err,
                                value: parts[0].into(),
                            })?;
                    properties.insert(cpu::CPU_QUOTA, Value::U64(quota));

                    if parts.len() == 2 {
                        let period = parts[1].parse::<u64>().map_err(|err| {
                            SystemdUnifiedError::CpuPeriod {
                                err,
                                value: parts[1].into(),
                            }
                        })?;
                        properties.insert(cpu::CPU_PERIOD, Value::U64(period));
                    }
                }
                cpuset @ ("cpuset.cpus" | "cpuset.mems") => {
                    if systemd_version <= 243 {
                        return Err(SystemdUnifiedError::OldSystemd(cpuset.into()));
                    }

                    let bitmask = to_bitmask(value).map_err(SystemdUnifiedError::CpuSetCpu)?;

                    let systemd_cpuset = match cpuset {
                        "cpuset.cpus" => cpuset::ALLOWED_CPUS,
                        "cpuset.mems" => cpuset::ALLOWED_NODES,
                        file_name => unreachable!("{} was not matched", file_name),
                    };

                    properties.insert(
                        systemd_cpuset,
                        Value::Array(zbus::zvariant::Array::from(bitmask)),
                    );
                }
                memory @ ("memory.min" | "memory.low" | "memory.high" | "memory.max") => {
                    let value =
                        value
                            .parse::<u64>()
                            .map_err(|err| SystemdUnifiedError::Memory {
                                err,
                                name: memory.into(),
                                value: value.into(),
                            })?;
                    let systemd_memory = match memory {
                        "memory.min" => memory::MEMORY_MIN,
                        "memory.low" => memory::MEMORY_LOW,
                        "memory.high" => memory::MEMORY_HIGH,
                        "memory.max" => memory::MEMORY_MAX,
                        file_name => unreachable!("{} was not matched", file_name),
                    };
                    properties.insert(systemd_memory, Value::U64(value));
                }
                "pids.max" => {
                    let pids = value.trim().parse::<i64>().map_err(|err| {
                        SystemdUnifiedError::PidsMax {
                            err,
                            value: value.into(),
                        }
                    })?;
                    properties.insert(pids::TASKS_MAX, Value::U64(pids as u64));
                }

                unknown => tracing::warn!("could not apply {}. Unknown property.", unknown),
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{bail, Context, Result};
    use zbus::zvariant::Value;

    use super::*;

    #[test]
    fn test_set() -> Result<()> {
        // arrange
        let unified: HashMap<String, String> = [
            ("cpu.weight", "22000"),
            ("cpuset.cpus", "0-3"),
            ("cpuset.mems", "0-3"),
            ("memory.min", "100000"),
            ("memory.low", "200000"),
            ("memory.high", "300000"),
            ("memory.max", "400000"),
            ("pids.max", "100"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();

        let mut expected: HashMap<&str, Value> = HashMap::new();
        expected.insert(cpu::CPU_WEIGHT, Value::U64(840u64));
        expected.insert(
            cpuset::ALLOWED_CPUS,
            Value::Array(zbus::zvariant::Array::from(vec![15u8])),
        );
        expected.insert(
            cpuset::ALLOWED_NODES,
            Value::Array(zbus::zvariant::Array::from(vec![15u8])),
        );
        expected.insert(memory::MEMORY_MIN, Value::U64(100000u64));
        expected.insert(memory::MEMORY_LOW, Value::U64(200000u64));
        expected.insert(memory::MEMORY_HIGH, Value::U64(300000u64));
        expected.insert(memory::MEMORY_MAX, Value::U64(400000u64));
        expected.insert(pids::TASKS_MAX, Value::U64(100u64));

        // act
        let mut actual: HashMap<&str, Value> = HashMap::new();
        Unified::apply(&unified, 245, &mut actual).context("apply unified")?;

        // assert
        for (setting, value) in expected {
            assert!(actual.contains_key(setting));
            match (value, &actual[setting]) {
                (Value::U64(expected), Value::U64(actual)) => {
                    assert_eq!(expected, *actual, "{setting}")
                }
                (Value::Array(expected), Value::Array(actual)) => {
                    let _expected = expected.iter().next().unwrap();
                    let _actual = actual.iter().next().unwrap();
                    match (_expected, _actual) {
                        (Value::U8(__expected), Value::U8(__actual)) => {
                            assert_eq!(__expected, __actual)
                        }
                        arg_type => bail!("unexpected arg type here {:?}", arg_type),
                    }
                }
                arg_type => bail!("unexpected arg type {:?}", arg_type),
            }
        }

        Ok(())
    }

    #[test]
    fn test_cpu_max_quota_and_period() -> Result<()> {
        // arrange
        let unified: HashMap<String, String> = [("cpu.max", "500000 250000")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let mut actual: HashMap<&str, Value> = HashMap::new();

        // act
        Unified::apply(&unified, 245, &mut actual).context("apply unified")?;

        // assert
        assert!(actual.contains_key(cpu::CPU_PERIOD));
        assert!(actual.contains_key(cpu::CPU_QUOTA));

        assert!(matches!(actual[cpu::CPU_PERIOD], Value::U64(250000)));
        assert!(matches!(actual[cpu::CPU_QUOTA], Value::U64(500000)));

        Ok(())
    }

    #[test]
    fn test_cpu_max_quota_only() -> Result<()> {
        // arrange
        let unified: HashMap<String, String> = [("cpu.max", "500000")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let mut actual: HashMap<&str, Value> = HashMap::new();

        // act
        Unified::apply(&unified, 245, &mut actual).context("apply unified")?;

        // assert
        assert!(!actual.contains_key(cpu::CPU_PERIOD));
        assert!(actual.contains_key(cpu::CPU_QUOTA));

        assert!(matches!(actual[cpu::CPU_QUOTA], Value::U64(500000)));

        Ok(())
    }
}
