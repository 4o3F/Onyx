use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct PreflightSummary {
    pub contest_id: String,
    pub problems: Vec<ContestProblemInfo>,
    pub problem_short_names: Vec<String>,
    pub solutions_root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ContestProblemInfo {
    pub id: u32,
    pub short_name: String,
}

#[derive(Debug, Deserialize)]
struct ContestSummary {
    id: String,
    start_time: Option<String>,
    end_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContestProblemSummary {
    probid: u32,
    short_name: String,
}

pub async fn run_preflight_checks(
    config: &Config,
    config_dir: Option<&Path>,
) -> anyhow::Result<PreflightSummary> {
    validate_required_config_fields(config)?;
    validate_contest_window(config).await?;

    let problems = fetch_contest_problems(config).await?;
    let solutions_root = validate_solutions_layout(config_dir, &config.solutions_path, &problems)?;

    let problem_short_names = problems
        .iter()
        .map(|problem| problem.short_name.clone())
        .collect::<Vec<_>>();
    let problems = problems
        .iter()
        .map(|problem| ContestProblemInfo {
            id: problem.probid,
            short_name: problem.short_name.clone(),
        })
        .collect::<Vec<_>>();

    Ok(PreflightSummary {
        contest_id: config.contest_id.clone(),
        problems,
        problem_short_names,
        solutions_root,
    })
}

fn validate_required_config_fields(config: &Config) -> anyhow::Result<()> {
    if config.base_url.trim().is_empty() {
        bail!("config.base_url is required");
    }
    if config.team_csv.trim().is_empty() {
        bail!("config.team_csv is required");
    }
    if config.contest_id.trim().is_empty() {
        bail!("config.contest_id is required");
    }
    if config.solutions_path.trim().is_empty() {
        bail!("config.solutions_path is required");
    }
    if !(0.0..=1.0).contains(&config.wrong_solution_probability) {
        bail!("config.wrong_solution_probability must be in range [0.0, 1.0]");
    }

    Ok(())
}

async fn validate_contest_window(config: &Config) -> anyhow::Result<()> {
    let contests_url = format!("{}/api/v4/contests", config.base_url.trim_end_matches('/'));

    let response = reqwest::Client::new()
        .get(&contests_url)
        .send()
        .await
        .with_context(|| format!("failed to request contests API at {contests_url}"))?;

    let status = response.status();
    if !status.is_success() {
        bail!(
            "contests API returned HTTP {} for {}",
            status.as_u16(),
            contests_url
        );
    }

    let contests = response
        .json::<Vec<ContestSummary>>()
        .await
        .with_context(|| format!("failed to parse contests API response from {contests_url}"))?;

    let contest = contests
        .into_iter()
        .find(|contest| contest.id == config.contest_id)
        .ok_or_else(|| {
            anyhow!(
                "contest id '{}' not found in {}",
                config.contest_id,
                contests_url
            )
        })?;

    let now = Utc::now();
    let start_raw = contest
        .start_time
        .as_deref()
        .ok_or_else(|| anyhow!("contest '{}' has no start_time", contest.id))?;
    let start_time = parse_rfc3339_utc(start_raw, "start_time", &contest.id)?;

    if now < start_time {
        bail!(
            "contest '{}' has not started yet (start_time: {})",
            contest.id,
            start_raw
        );
    }

    if let Some(end_raw) = contest.end_time.as_deref() {
        let end_time = parse_rfc3339_utc(end_raw, "end_time", &contest.id)?;
        if now >= end_time {
            bail!(
                "contest '{}' has already ended (end_time: {})",
                contest.id,
                end_raw
            );
        }
    }

    Ok(())
}

async fn fetch_contest_problems(config: &Config) -> anyhow::Result<Vec<ContestProblemSummary>> {
    let problems_url = format!(
        "{}/api/v4/contests/{}/problems",
        config.base_url.trim_end_matches('/'),
        config.contest_id
    );

    let response = reqwest::Client::new()
        .get(&problems_url)
        .send()
        .await
        .with_context(|| format!("failed to request contest problems API at {problems_url}"))?;

    let status = response.status();
    if !status.is_success() {
        bail!(
            "contest problems API returned HTTP {} for {}",
            status.as_u16(),
            problems_url
        );
    }

    let problems = response
        .json::<Vec<ContestProblemSummary>>()
        .await
        .with_context(|| {
            format!("failed to parse contest problems API response from {problems_url}")
        })?;

    if problems.is_empty() {
        bail!("contest '{}' currently has no problems", config.contest_id);
    }

    for problem in &problems {
        if problem.probid == 0 {
            bail!("contest '{}' has a problem with empty id", config.contest_id);
        }
        if problem.short_name.trim().is_empty() {
            bail!(
                "contest '{}' has a problem with empty short_name",
                config.contest_id
            );
        }
    }

    Ok(problems)
}

fn validate_solutions_layout(
    config_dir: Option<&Path>,
    solutions_path: &str,
    problems: &[ContestProblemSummary],
) -> anyhow::Result<PathBuf> {
    let solutions_root = resolve_path(config_dir, solutions_path);
    if !solutions_root.exists() {
        bail!(
            "solutions_path does not exist: {}",
            solutions_root.display()
        );
    }
    if !solutions_root.is_dir() {
        bail!(
            "solutions_path is not a directory: {}",
            solutions_root.display()
        );
    }

    let mut issues = Vec::new();
    for problem in problems {
        let problem_dir = solutions_root.join(problem.short_name.trim());
        if !problem_dir.exists() {
            issues.push(format!(
                "missing problem directory '{}' under {}",
                problem.short_name,
                solutions_root.display()
            ));
            continue;
        }
        if !problem_dir.is_dir() {
            issues.push(format!(
                "problem path '{}' is not a directory",
                problem_dir.display()
            ));
            continue;
        }

        let source_count = count_c_or_cpp_files(&problem_dir).with_context(|| {
            format!(
                "failed to inspect source files in '{}'",
                problem_dir.display()
            )
        })?;
        if !source_count.unknown_named_sources.is_empty() {
            issues.push(format!(
                "problem '{}' has invalid source file names in {}: {} (only AC/TLE are allowed)",
                problem.short_name,
                problem_dir.display(),
                source_count.unknown_named_sources.join(", ")
            ));
        }
        if !source_count.has_ac {
            issues.push(format!(
                "problem '{}' is missing AC.c or AC.cpp in {}",
                problem.short_name,
                problem_dir.display()
            ));
        }
        if !source_count.has_tle {
            issues.push(format!(
                "problem '{}' is missing TLE.c or TLE.cpp in {}",
                problem.short_name,
                problem_dir.display()
            ));
        }
    }

    if !issues.is_empty() {
        bail!(
            "pre-flight solutions validation failed:\n{}",
            issues.join("\n")
        );
    }

    Ok(solutions_root)
}

struct SolutionNameCheck {
    has_ac: bool,
    has_tle: bool,
    unknown_named_sources: Vec<String>,
}

fn count_c_or_cpp_files(dir: &Path) -> anyhow::Result<SolutionNameCheck> {
    let mut has_ac = false;
    let mut has_tle = false;
    let mut unknown_named_sources = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        let is_c_or_cpp = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("c") || ext.eq_ignore_ascii_case("cpp"));
        if is_c_or_cpp {
            match path.file_stem().and_then(|stem| stem.to_str()) {
                Some(stem) if stem.eq_ignore_ascii_case("ac") => has_ac = true,
                Some(stem) if stem.eq_ignore_ascii_case("tle") => has_tle = true,
                Some(stem) => unknown_named_sources.push(stem.to_string()),
                None => unknown_named_sources.push(path.display().to_string()),
            }
        }
    }

    Ok(SolutionNameCheck {
        has_ac,
        has_tle,
        unknown_named_sources,
    })
}

fn resolve_path(config_dir: Option<&Path>, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return path;
    }

    match config_dir {
        Some(dir) => dir.join(path),
        None => path,
    }
}

fn parse_rfc3339_utc(value: &str, field: &str, contest_id: &str) -> anyhow::Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid {} '{}' for contest '{}'", field, value, contest_id))?;
    Ok(parsed.with_timezone(&Utc))
}
