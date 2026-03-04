use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct TeamCredential {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
struct TeamCsvRow {
    #[serde(default, rename = "id")]
    _id: Option<String>,
    username: String,
    password: String,
}

pub fn load_team_credentials(path: &Path) -> anyhow::Result<Vec<TeamCredential>> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .with_context(|| format!("failed to open team_csv file: {}", path.display()))?;

    let mut team_credentials = Vec::new();
    for (row_index, row) in reader.deserialize::<TeamCsvRow>().enumerate() {
        let row = row.with_context(|| {
            format!(
                "failed to parse team_csv row {} in {}",
                row_index + 2,
                path.display()
            )
        })?;

        let username = row.username.trim();
        let password = row.password.trim();

        if username.is_empty() {
            bail!(
                "team_csv row {} has empty username in {}",
                row_index + 2,
                path.display()
            );
        }
        if password.is_empty() {
            bail!(
                "team_csv row {} has empty password in {}",
                row_index + 2,
                path.display()
            );
        }

        team_credentials.push(TeamCredential {
            username: username.to_string(),
            password: password.to_string(),
        });
    }

    if team_credentials.is_empty() {
        bail!(
            "team_csv file {} does not contain any usable rows",
            path.display()
        );
    }

    Ok(team_credentials)
}
