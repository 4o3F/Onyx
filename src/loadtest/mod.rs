mod participant;
mod flows;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{anyhow, bail};
use goose::prelude::*;
use rand::Rng;
use tokio::sync::Notify;

use crate::models::TeamCredential;
use crate::preflight::ContestProblemInfo;

pub use participant::participant_user_flow;

#[derive(Clone)]
struct LoadTestContext {
    team_credentials: Vec<TeamCredential>,
    contest_problems: Vec<ContestProblemAsset>,
    wrong_solution_probability: f64,
    login_gate: Arc<LoginGate>,
}

#[derive(Debug, Clone)]
pub(super) struct ContestProblemAsset {
    pub id: u32,
    pub short_name: String,
    pub ac_filename: String,
    pub ac_code: String,
    pub tle_filename: String,
    pub tle_code: String,
}

struct LoginGate {
    total_users: usize,
    logged_in_users: AtomicUsize,
    login_failed: AtomicBool,
    notify: Notify,
}

#[derive(Debug, Clone)]
struct AssignedUser {
    username: String,
    password: String,
    login_completed: bool,
    session_cookie: Option<String>,
    accepted_problem_ids: HashSet<u32>,
    last_solution_kind: Option<SolutionKind>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SolutionKind {
    Ac,
    Tle,
}

static LOAD_TEST_CONTEXT: OnceLock<LoadTestContext> = OnceLock::new();
static PARTICIPANT_INDEX: AtomicUsize = AtomicUsize::new(0);

pub fn init_load_test_context(
    team_credentials: Vec<TeamCredential>,
    contest_problems: Vec<ContestProblemInfo>,
    solutions_root: PathBuf,
    wrong_solution_probability: f64,
) -> anyhow::Result<()> {
    if team_credentials.is_empty() {
        bail!("team_csv must contain at least one team account");
    }
    if contest_problems.is_empty() {
        bail!("contest must contain at least one problem");
    }
    let contest_problems = load_contest_problem_assets(&contest_problems, &solutions_root)?;
    PARTICIPANT_INDEX.store(0, Ordering::Relaxed);

    LOAD_TEST_CONTEXT
        .set(LoadTestContext {
            login_gate: Arc::new(LoginGate {
                total_users: team_credentials.len(),
                logged_in_users: AtomicUsize::new(0),
                login_failed: AtomicBool::new(false),
                notify: Notify::new(),
            }),
            team_credentials,
            contest_problems,
            wrong_solution_probability,
        })
        .map_err(|_| anyhow!("load test context can only be initialized once"))?;

    Ok(())
}

fn load_test_context() -> &'static LoadTestContext {
    LOAD_TEST_CONTEXT
        .get()
        .expect("load test context is not initialized")
}

pub(super) async fn prepare_user(user: &mut GooseUser) -> TransactionResult {
    if user.get_session_data::<AssignedUser>().is_some() {
        return Ok(());
    }

    let context = load_test_context();
    let index = PARTICIPANT_INDEX.fetch_add(1, Ordering::Relaxed);
    let credential = &context.team_credentials[index];
    let assigned = AssignedUser {
        username: credential.username.clone(),
        password: credential.password.clone(),
        login_completed: false,
        session_cookie: None,
        accepted_problem_ids: HashSet::new(),
        last_solution_kind: None,
    };

    // Keep per-user assignment for future request implementation.
    user.set_session_data(assigned);

    // Touch fields so the stored assignment stays warning-free even before concrete flows are added.
    let assigned = user.get_session_data_unchecked::<AssignedUser>();
    let _ = (
        assigned.username.len(),
        assigned.password.len(),
        assigned.login_completed,
        assigned.session_cookie.as_deref(),
        assigned.accepted_problem_ids.len(),
        assigned.last_solution_kind,
    );

    Ok(())
}

pub(super) fn decide_participant_solution(user: &mut GooseUser) -> SolutionKind {
    let context = load_test_context();
    let assigned = user.get_session_data_unchecked_mut::<AssignedUser>();

    let roll = rand::random::<f64>();
    let solution_kind = if roll < context.wrong_solution_probability {
        SolutionKind::Tle
    } else {
        SolutionKind::Ac
    };
    assigned.last_solution_kind = Some(solution_kind);
    solution_kind
}

pub(super) fn pick_random_unsolved_problem(user: &GooseUser) -> Option<ContestProblemAsset> {
    let context = load_test_context();
    let assigned = user.get_session_data_unchecked::<AssignedUser>();

    let unsolved = context
        .contest_problems
        .iter()
        .filter(|problem| !assigned.accepted_problem_ids.contains(&problem.id))
        .cloned()
        .collect::<Vec<_>>();
    if unsolved.is_empty() {
        return None;
    }

    let mut rng = rand::rng();
    let picked_index = rng.random_range(0..unsolved.len());
    Some(unsolved[picked_index].clone())
}

pub(super) fn mark_problem_accepted(user: &mut GooseUser, problem_id: u32) {
    let assigned = user.get_session_data_unchecked_mut::<AssignedUser>();
    assigned.accepted_problem_ids.insert(problem_id);
}

pub(super) fn all_problems_accepted(user: &GooseUser) -> bool {
    let context = load_test_context();
    let assigned = user.get_session_data_unchecked::<AssignedUser>();
    assigned.accepted_problem_ids.len() >= context.contest_problems.len()
}

fn load_contest_problem_assets(
    contest_problems: &[ContestProblemInfo],
    solutions_root: &Path,
) -> anyhow::Result<Vec<ContestProblemAsset>> {
    let mut assets = Vec::with_capacity(contest_problems.len());
    for problem in contest_problems {
        let problem_dir = solutions_root.join(problem.short_name.trim());
        let (ac_filename, ac_code) = load_solution_source_file(&problem_dir, "ac")?;
        let (tle_filename, tle_code) = load_solution_source_file(&problem_dir, "tle")?;
        assets.push(ContestProblemAsset {
            id: problem.id,
            short_name: problem.short_name.clone(),
            ac_filename,
            ac_code,
            tle_filename,
            tle_code,
        });
    }
    Ok(assets)
}

fn load_solution_source_file(problem_dir: &Path, stem: &str) -> anyhow::Result<(String, String)> {
    for ext in ["cpp", "c"] {
        let candidate = problem_dir.join(format!("{}.{}", stem.to_uppercase(), ext));
        if candidate.exists() && candidate.is_file() {
            let filename = candidate
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let source = fs::read_to_string(&candidate).map_err(|error| {
                anyhow!(
                    "failed to read solution file '{}': {error}",
                    candidate.display()
                )
            })?;
            return Ok((filename, source));
        }
    }

    bail!(
        "missing {}.c or {}.cpp in {}",
        stem.to_uppercase(),
        stem.to_uppercase(),
        problem_dir.display()
    )
}

pub(super) fn mark_login_success() -> usize {
    let gate = &load_test_context().login_gate;
    let current = gate.logged_in_users.fetch_add(1, Ordering::SeqCst) + 1;
    gate.notify.notify_waiters();
    current
}

pub(super) fn mark_login_failed() {
    let gate = &load_test_context().login_gate;
    gate.login_failed.store(true, Ordering::SeqCst);
    gate.notify.notify_waiters();
}

pub(super) async fn wait_for_all_logins() -> anyhow::Result<()> {
    let gate = &load_test_context().login_gate;
    loop {
        if gate.login_failed.load(Ordering::SeqCst) {
            bail!("a participant login failed, aborting synchronized phase");
        }
        if gate.logged_in_users.load(Ordering::SeqCst) >= gate.total_users {
            return Ok(());
        }
        gate.notify.notified().await;
    }
}
