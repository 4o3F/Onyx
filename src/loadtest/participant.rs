use goose::prelude::*;
use reqwest::Method;
use reqwest::header::COOKIE;
use tokio::time::{Duration, sleep};
use rand::Rng;

use super::flows::login::ensure_logged_in_and_synced;
use super::flows::submission::submit_problem_once;
use super::{
    AssignedUser, SolutionKind, all_problems_accepted, decide_participant_solution,
    mark_problem_accepted, pick_random_unsolved_problem, prepare_user,
};

const MAX_SUBMISSION_ATTEMPTS: usize = 20_000;

pub async fn participant_user_flow(user: &mut GooseUser) -> TransactionResult {
    prepare_user(user).await?;
    let assigned_team = match user.get_session_data::<AssignedUser>() {
        Some(team) => team.clone(),
        None => {
            tracing::error!("missing AssignedUser session data; exiting current Goose user");
            user.config.iterations = user.get_iterations() + 1;
            return Err(Box::new(TransactionError::InvalidMethod {
                method: Method::GET,
            }));
        }
    };

    ensure_logged_in_and_synced(user, &assigned_team).await?;

    let assigned_team = match user.get_session_data::<AssignedUser>() {
        Some(team) => team.clone(),
        None => {
            tracing::error!("missing AssignedUser session after login; exiting current Goose user");
            user.config.iterations = user.get_iterations() + 1;
            return Err(Box::new(TransactionError::InvalidMethod {
                method: Method::GET,
            }));
        }
    };

    if let Err(error) = random_wait_up_to(&assigned_team.username, 30, "after login").await {
        user.config.iterations = user.get_iterations() + 1;
        return Err(error);
    }
    if let Err(error) = get_with_session_cookie(user, &assigned_team, "/team").await {
        user.config.iterations = user.get_iterations() + 1;
        return Err(error);
    }

    let mut attempts = 0usize;
    while !all_problems_accepted(user) {
        attempts += 1;
        if attempts > MAX_SUBMISSION_ATTEMPTS {
            tracing::error!(
                "user {} exceeded submission attempt limit ({MAX_SUBMISSION_ATTEMPTS})",
                assigned_team.username
            );
            user.config.iterations = user.get_iterations() + 1;
            return Err(Box::new(TransactionError::InvalidMethod {
                method: Method::POST,
            }));
        }

        if let Err(error) = random_wait_up_to(&assigned_team.username, 10, "before submit").await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }
        let Some(problem) = pick_random_unsolved_problem(user) else {
            break;
        };
        let chosen_solution = decide_participant_solution(user);
        let chosen_text = match chosen_solution {
            SolutionKind::Ac => "AC",
            SolutionKind::Tle => "TLE",
        };
        tracing::info!(
            "user {} selected problem {} (id={}) with {}",
            assigned_team.username,
            problem.short_name,
            problem.id,
            chosen_text
        );

        if let Err(error) = submit_problem_once(user, &assigned_team, &problem, chosen_solution).await
        {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }
        if matches!(chosen_solution, SolutionKind::Ac) {
            mark_problem_accepted(user, problem.id);
        }

        if let Err(error) = random_wait_up_to(&assigned_team.username, 10, "before first scoreboard").await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }
        if let Err(error) = get_with_session_cookie(user, &assigned_team, "/team/scoreboard").await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }

        if let Err(error) = random_wait_up_to(&assigned_team.username, 10, "before random /team visits (max10)").await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }
        if let Err(error) = random_get_team(user, &assigned_team, 10).await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }

        if let Err(error) = random_wait_up_to(&assigned_team.username, 20, "before second scoreboard").await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }
        if let Err(error) = get_with_session_cookie(user, &assigned_team, "/team/scoreboard").await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }

        if let Err(error) = random_get_team(user, &assigned_team, 5).await {
            user.config.iterations = user.get_iterations() + 1;
            return Err(error);
        }
    }

    user.config.iterations = user.get_iterations() + 1;

    Ok(())
}

async fn random_wait_up_to(username: &str, max_seconds: u64, stage: &str) -> TransactionResult {
    let wait_ms = {
        let mut rng = rand::rng();
        rng.random_range(0..=(max_seconds * 1000))
    };
    tracing::info!(
        "user {} waiting {}ms at stage '{}'",
        username,
        wait_ms,
        stage
    );
    if wait_ms == 0 {
        return Ok(());
    }

    sleep(Duration::from_millis(wait_ms)).await;
    Ok(())
}

async fn random_get_team(
    user: &mut GooseUser,
    assigned_team: &AssignedUser,
    max_visits: usize,
) -> TransactionResult {
    let visits = {
        let mut rng = rand::rng();
        rng.random_range(0..=max_visits)
    };
    tracing::debug!(
        "user {} random GET /team visits: {} (max {})",
        assigned_team.username,
        visits,
        max_visits
    );
    for _ in 0..visits {
        get_with_session_cookie(user, assigned_team, "/team").await?;
    }
    Ok(())
}

async fn get_with_session_cookie(
    user: &mut GooseUser,
    assigned_team: &AssignedUser,
    path: &str,
) -> TransactionResult {
    tracing::info!(
        "user {} requesting path {}",
        assigned_team.username,
        path
    );
    let session_cookie = match assigned_team.session_cookie.as_deref() {
        Some(cookie) if !cookie.is_empty() => cookie,
        _ => {
            return Err(Box::new(TransactionError::InvalidMethod {
                method: Method::GET,
            }));
        }
    };

    let request_builder = user
        .get_request_builder(&GooseMethod::Get, path)?
        .header(COOKIE, session_cookie);
    let goose_request = GooseRequest::builder()
        .method(GooseMethod::Get)
        .path(path)
        .set_request_builder(request_builder)
        .build();

    let mut response = user.request(goose_request).await?;
    let http_response = match response.response {
        Ok(resp) => resp,
        Err(error) => {
            tracing::error!("GET {} failed: {}", path, error);
            return Err(Box::new(TransactionError::InvalidMethod {
                method: Method::GET,
            }));
        }
    };
    if !http_response.status().is_success() {
        return user.set_failure(
            "GET page returned non-success status",
            &mut response.request,
            Some(http_response.headers()),
            None,
        );
    }
    Ok(())
}
