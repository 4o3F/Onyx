mod config;
mod loadtest;
mod models;
mod preflight;

use std::path::{Path, PathBuf};
use std::{io, io::Write};

use clap::Parser;
use config::{Config, load_config};
use goose::config::GooseConfiguration;
use goose::prelude::*;
use loadtest::participant_user_flow;
use models::load_team_credentials;
use preflight::{PreflightSummary, run_preflight_checks};

#[derive(Debug, Parser)]
#[command(name = "onyx")]
struct Cli {
    /// Path to a TOML config file.
    #[arg(short, long, value_name = "PATH")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let (config, config_dir) = load_app_config(&cli.config)?;

    let subscriber = tracing_subscriber::fmt()
        .with_max_level(config.logging.level)
        .with_level(true)
        .finish();
    if let Err(error) = tracing::subscriber::set_global_default(subscriber) {
        eprintln!("failed to initialize tracing subscriber: {error}");
        return Ok(());
    }

    let preflight_summary = match run_preflight_checks(&config, config_dir.as_deref()).await {
        Ok(summary) => summary,
        Err(error) => {
            tracing::error!("pre-flight check failed: {error:#}");
            return Ok(());
        }
    };

    let team_csv_path = resolve_team_csv_path(config_dir.as_deref(), &config.team_csv);
    let team_credentials = match load_team_credentials(&team_csv_path) {
        Ok(credentials) => credentials,
        Err(error) => {
            tracing::error!(
                "failed to load team CSV '{}': {error:#}",
                team_csv_path.display()
            );
            return Ok(());
        }
    };
    let participant_users = team_credentials.len();
    if !confirm_start_load_test(
        &preflight_summary,
        &team_csv_path,
        participant_users,
        config.wrong_solution_probability,
    )? {
        tracing::info!("load test canceled by user confirmation step");
        return Ok(());
    }

    loadtest::init_load_test_context(
        team_credentials,
        preflight_summary.problems.clone(),
        preflight_summary.solutions_root.clone(),
        config.wrong_solution_probability,
    )?;

    let mut goose_config = GooseConfiguration::default();
    goose_config.host = config.base_url.clone();
    goose_config.users = Some(participant_users);
    // Launch all users within ~1s to generate a near-instant spike.
    goose_config.hatch_rate = Some(participant_users.to_string());
    goose_config.quiet = 1;
    goose_config.no_print_metrics = true;
    goose_config.no_error_summary = true;
    goose_config.request_body = true;
    goose_config.report_file = vec![String::from("report.html")];

    let mut attack = GooseAttack::initialize_with_config(goose_config)?;
    attack = attack.register_scenario(
        scenario!("ParticipantUsers")
            .set_weight(participant_users)?
            .register_transaction(transaction!(participant_user_flow)),
    );
    attack.execute().await?;

    Ok(())
}

fn load_app_config(config_path: &Path) -> anyhow::Result<(Config, Option<PathBuf>)> {
    Ok((
        load_config(config_path)?,
        config_path.parent().map(std::path::Path::to_path_buf),
    ))
}

fn resolve_team_csv_path(config_dir: Option<&Path>, team_csv: &str) -> PathBuf {
    let path = PathBuf::from(team_csv);
    if path.is_absolute() {
        return path;
    }

    match config_dir {
        Some(dir) => dir.join(path),
        None => path,
    }
}

fn confirm_start_load_test(
    preflight_summary: &PreflightSummary,
    team_csv_path: &Path,
    participant_users: usize,
    wrong_solution_probability: f64,
) -> anyhow::Result<bool> {
    tracing::info!("pre-flight checks passed");
    tracing::info!("contest id (cid): {}", preflight_summary.contest_id);
    tracing::info!(
        "problems (short_name): {}",
        preflight_summary.problem_short_names.join(", ")
    );
    let problem_mapping = preflight_summary
        .problems
        .iter()
        .map(|p| format!("{}=>{}", p.short_name, p.id))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!("problem mapping (short_name=>id): {problem_mapping}");
    tracing::info!(
        "solutions root: {}",
        preflight_summary.solutions_root.display()
    );
    tracing::info!("team CSV: {}", team_csv_path.display());
    tracing::info!("participant users (from CSV): {participant_users}");
    tracing::info!("total Goose users: {}", participant_users);
    tracing::info!("wrong solution probability (TLE): {wrong_solution_probability}");
    tracing::info!("type 'YES' to start Goose load test, any other input will cancel");

    print!("Start load test now? Type YES to continue: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().eq_ignore_ascii_case("YES"))
}
