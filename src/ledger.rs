use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    goal: String,
    requirements: Vec<RequirementStatus>,
    unresolved_requirement_ids: Vec<String>,
    closed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequirementStatus {
    id: String,
    text: String,
    evidence: Option<String>,
}

impl RequirementStatus {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn evidence(&self) -> Option<&str> {
        self.evidence.as_deref()
    }
}

impl Snapshot {
    #[must_use]
    pub fn goal(&self) -> &str {
        &self.goal
    }

    #[must_use]
    pub fn requirements(&self) -> &[RequirementStatus] {
        &self.requirements
    }

    #[must_use]
    pub fn unresolved_requirement_ids(&self) -> &[String] {
        &self.unresolved_requirement_ids
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerError(String);

impl LedgerError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LedgerError {}

impl From<io::Error> for LedgerError {
    fn from(error: io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for LedgerError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClosureError {
    UnresolvedRequirements(Vec<String>),
    Ledger(String),
}

impl fmt::Display for ClosureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedRequirements(ids) => {
                write!(formatter, "unresolved requirements: {}", ids.join(", "))
            }
            Self::Ledger(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ClosureError {}

impl From<LedgerError> for ClosureError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error.to_string())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Requirement {
    id: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Record {
    schema_version: u32,
    sequence: u64,
    #[serde(flatten)]
    event: Event,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    RunStarted {
        goal: String,
        requirements: Vec<Requirement>,
    },
    SteeringAdded {
        requirement: Requirement,
    },
    RequirementSatisfied {
        requirement_id: String,
        evidence: String,
    },
    RunClosed,
}

#[derive(Debug)]
struct FoldedState {
    goal: String,
    requirements: BTreeMap<String, RequirementState>,
    closed: bool,
}

#[derive(Debug)]
struct RequirementState {
    text: String,
    evidence: Option<String>,
}

impl Ledger {
    pub fn create<P, G, I, ID, T>(path: P, goal: G, requirements: I) -> Result<Self, LedgerError>
    where
        P: AsRef<Path>,
        G: Into<String>,
        I: IntoIterator<Item = (ID, T)>,
        ID: Into<String>,
        T: Into<String>,
    {
        let goal = non_empty(goal.into(), "goal")?;
        let requirements = requirements
            .into_iter()
            .map(|(id, text)| {
                Ok(Requirement {
                    id: non_empty(id.into(), "requirement id")?,
                    text: non_empty(text.into(), "requirement text")?,
                })
            })
            .collect::<Result<Vec<_>, LedgerError>>()?;
        validate_unique_requirements(&requirements)?;
        if requirements.is_empty() {
            return Err(LedgerError::new("at least one requirement is required"));
        }

        let ledger = Self {
            path: path.as_ref().to_path_buf(),
        };
        let record = Record {
            schema_version: SCHEMA_VERSION,
            sequence: 1,
            event: Event::RunStarted { goal, requirements },
        };
        ledger.create_with_record(&record)?;
        Ok(ledger)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let ledger = Self {
            path: path.as_ref().to_path_buf(),
        };
        ledger.records()?;
        Ok(ledger)
    }

    pub fn steer(&mut self, requirement: impl Into<String>) -> Result<String, LedgerError> {
        let requirement = non_empty(requirement.into(), "steering requirement")?;
        let state = self.fold()?;
        ensure_open(&state)?;

        let mut suffix = state.requirements.len() + 1;
        let id = loop {
            let candidate = format!("R{suffix}");
            if !state.requirements.contains_key(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        self.append(Event::SteeringAdded {
            requirement: Requirement {
                id: id.clone(),
                text: requirement,
            },
        })?;
        Ok(id)
    }

    pub fn satisfy(
        &mut self,
        requirement_id: &str,
        evidence: impl Into<String>,
    ) -> Result<(), LedgerError> {
        let requirement_id = non_empty(requirement_id.to_owned(), "requirement id")?;
        let evidence = non_empty(evidence.into(), "evidence")?;
        let state = self.fold()?;
        ensure_open(&state)?;
        if !state.requirements.contains_key(&requirement_id) {
            return Err(LedgerError::new(format!(
                "unknown requirement: {requirement_id}"
            )));
        }
        self.append(Event::RequirementSatisfied {
            requirement_id,
            evidence,
        })
    }

    pub fn snapshot(&self) -> Result<Snapshot, LedgerError> {
        let state = self.fold()?;
        let requirements: Vec<RequirementStatus> = state
            .requirements
            .into_iter()
            .map(|(id, requirement)| RequirementStatus {
                id,
                text: requirement.text,
                evidence: requirement.evidence,
            })
            .collect();
        let unresolved_requirement_ids = requirements
            .iter()
            .filter(|requirement| requirement.evidence.is_none())
            .map(|requirement| requirement.id.clone())
            .collect();
        Ok(Snapshot {
            goal: state.goal,
            requirements,
            unresolved_requirement_ids,
            closed: state.closed,
        })
    }

    pub fn close(&mut self) -> Result<(), ClosureError> {
        let snapshot = self.snapshot()?;
        if snapshot.closed {
            return Ok(());
        }
        if !snapshot.unresolved_requirement_ids.is_empty() {
            return Err(ClosureError::UnresolvedRequirements(
                snapshot.unresolved_requirement_ids,
            ));
        }
        self.append(Event::RunClosed)?;
        Ok(())
    }

    fn create_with_record(&self, record: &Record) -> Result<(), LedgerError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)?;
        write_record(&mut file, record)?;
        file.sync_data()?;
        Ok(())
    }

    fn append(&self, event: Event) -> Result<(), LedgerError> {
        let records = self.records()?;
        let sequence = u64::try_from(records.len())
            .map_err(|_| LedgerError::new("ledger sequence exhausted"))?
            .checked_add(1)
            .ok_or_else(|| LedgerError::new("ledger sequence exhausted"))?;
        let record = Record {
            schema_version: SCHEMA_VERSION,
            sequence,
            event,
        };
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        write_record(&mut file, &record)?;
        file.sync_data()?;
        Ok(())
    }

    fn fold(&self) -> Result<FoldedState, LedgerError> {
        let records = self.records()?;
        let mut records = records.into_iter();
        let first = records
            .next()
            .ok_or_else(|| LedgerError::new("ledger is empty"))?;
        let Event::RunStarted { goal, requirements } = first.event else {
            return Err(LedgerError::new("ledger must begin with run_started"));
        };
        validate_unique_requirements(&requirements)?;
        let mut state = FoldedState {
            goal,
            requirements: requirements
                .into_iter()
                .map(|requirement| {
                    (
                        requirement.id,
                        RequirementState {
                            text: requirement.text,
                            evidence: None,
                        },
                    )
                })
                .collect(),
            closed: false,
        };

        for record in records {
            if state.closed {
                return Err(LedgerError::new("ledger contains events after closure"));
            }
            match record.event {
                Event::RunStarted { .. } => {
                    return Err(LedgerError::new("run_started may only appear first"));
                }
                Event::SteeringAdded { requirement } => {
                    if state.requirements.contains_key(&requirement.id) {
                        return Err(LedgerError::new(format!(
                            "duplicate requirement id: {}",
                            requirement.id
                        )));
                    }
                    state.requirements.insert(
                        requirement.id,
                        RequirementState {
                            text: requirement.text,
                            evidence: None,
                        },
                    );
                }
                Event::RequirementSatisfied {
                    requirement_id,
                    evidence,
                } => {
                    let requirement =
                        state.requirements.get_mut(&requirement_id).ok_or_else(|| {
                            LedgerError::new(format!("unknown requirement: {requirement_id}"))
                        })?;
                    requirement.evidence = Some(evidence);
                }
                Event::RunClosed => state.closed = true,
            }
        }
        Ok(state)
    }

    fn records(&self) -> Result<Vec<Record>, LedgerError> {
        let contents = fs::read_to_string(&self.path)?;
        if contents.is_empty() {
            return Err(LedgerError::new("ledger is empty"));
        }

        let mut records = Vec::new();
        for (index, line) in contents.lines().enumerate() {
            let record: Record = serde_json::from_str(line).map_err(|error| {
                LedgerError::new(format!("invalid ledger line {}: {error}", index + 1))
            })?;
            let expected = u64::try_from(index + 1)
                .map_err(|_| LedgerError::new("ledger sequence exhausted"))?;
            if record.schema_version != SCHEMA_VERSION {
                return Err(LedgerError::new(format!(
                    "unsupported schema version: {}",
                    record.schema_version
                )));
            }
            if record.sequence != expected {
                return Err(LedgerError::new(format!(
                    "invalid ledger sequence: expected {expected}, found {}",
                    record.sequence
                )));
            }
            records.push(record);
        }
        Ok(records)
    }
}

fn non_empty(value: String, field: &str) -> Result<String, LedgerError> {
    if value.trim().is_empty() {
        Err(LedgerError::new(format!("{field} must not be empty")))
    } else {
        Ok(value)
    }
}

fn validate_unique_requirements(requirements: &[Requirement]) -> Result<(), LedgerError> {
    let mut seen = BTreeMap::new();
    for requirement in requirements {
        if seen.insert(&requirement.id, ()).is_some() {
            return Err(LedgerError::new(format!(
                "duplicate requirement id: {}",
                requirement.id
            )));
        }
    }
    Ok(())
}

fn ensure_open(state: &FoldedState) -> Result<(), LedgerError> {
    if state.closed {
        Err(LedgerError::new("run is already closed"))
    } else {
        Ok(())
    }
}

fn write_record(file: &mut fs::File, record: &Record) -> Result<(), LedgerError> {
    serde_json::to_writer(&mut *file, record)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}
