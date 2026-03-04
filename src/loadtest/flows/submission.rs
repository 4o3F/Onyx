use goose::prelude::*;
use reqwest::Method;
use reqwest::header::COOKIE;
use reqwest::multipart;

use super::super::{AssignedUser, ContestProblemAsset, SolutionKind};

pub(in crate::loadtest) async fn submit_problem_once(
    user: &mut GooseUser,
    assigned_team: &AssignedUser,
    problem: &ContestProblemAsset,
    solution_kind: SolutionKind,
) -> TransactionResult {
    let session_cookie = match assigned_team.session_cookie.as_deref() {
        Some(cookie) if !cookie.is_empty() => cookie,
        _ => return submission_error("missing session cookie for submission"),
    };

    let submit_token = get_submit_token(user, session_cookie).await?;
    let (source_filename, source_code) = match solution_kind {
        SolutionKind::Ac => (&problem.ac_filename, &problem.ac_code),
        SolutionKind::Tle => (&problem.tle_filename, &problem.tle_code),
    };
    let language = if source_filename.to_ascii_lowercase().ends_with(".c") {
        "c"
    } else {
        "cpp"
    };

    let code_part = match multipart::Part::text(source_code.clone())
        .file_name(source_filename.clone())
        .mime_str("text/plain")
    {
        Ok(part) => part,
        Err(_) => return submission_error("invalid source mime type"),
    };
    let form = multipart::Form::new()
        .part("submit_problem[code][]", code_part)
        .text("submit_problem[problem]", problem.id.to_string())
        .text("submit_problem[language]", language.to_string())
        .text("submit_problem[entry_point]", String::new())
        .text("submit_problem[_token]", submit_token);

    let request_builder = user
        .get_request_builder(&GooseMethod::Post, "/team/submit")?
        .header(COOKIE, session_cookie)
        .multipart(form);
    let goose_request = GooseRequest::builder()
        .method(GooseMethod::Post)
        .path("/team/submit")
        .set_request_builder(request_builder)
        .build();
    let mut submit_response = user.request(goose_request).await?;
    let response = match submit_response.response {
        Ok(resp) => resp,
        Err(error) => return submission_error(&format!("POST /team/submit failed: {error}")),
    };

    if !response.status().is_success() && !response.status().is_redirection() {
        return user.set_failure(
            "POST /team/submit returned unexpected status",
            &mut submit_response.request,
            Some(response.headers()),
            None,
        );
    }

    tracing::info!(
        "user {} submitted {} for problem {} (id={})",
        assigned_team.username,
        source_filename,
        problem.short_name,
        problem.id
    );
    Ok(())
}

async fn get_submit_token(
    user: &mut GooseUser,
    session_cookie: &str,
) -> Result<String, Box<TransactionError>> {
    let submit_url = user.build_url("/team/submit")?;
    tracing::debug!("submit page request method=GET");
    tracing::debug!("submit page request url={submit_url}");
    tracing::debug!("submit page request header Cookie={session_cookie}");

    let request_builder = user
        .get_request_builder(&GooseMethod::Get, "/team/submit")?
        .header(COOKIE, session_cookie);
    let goose_request = GooseRequest::builder()
        .method(GooseMethod::Get)
        .path("/team/submit")
        .set_request_builder(request_builder)
        .build();
    let mut submit_page = user.request(goose_request).await?;
    let response = match submit_page.response {
        Ok(resp) => resp,
        Err(error) => return submission_error(&format!("GET /team/submit failed: {error}")),
    };

    if !response.status().is_success() {
        let _ = user.set_failure(
            "GET /team/submit returned non-success status",
            &mut submit_page.request,
            Some(response.headers()),
            None,
        );
        return submission_error("GET /team/submit returned non-success status");
    }

    let page_html = match response.text().await {
        Ok(text) => text,
        Err(error) => return submission_error(&format!("failed to read /team/submit html: {error}")),
    };
    match extract_submit_csrf_token(&page_html) {
        Some(token) => Ok(token),
        None => submission_error("missing submit_problem[_token] in /team/submit html"),
    }
}

fn extract_submit_csrf_token(html: &str) -> Option<String> {
    let id_marker = "id=\"submit_problem__token\"";
    let id_pos = html.find(id_marker)?;
    let tail = &html[id_pos..];

    let name_marker = "name=\"submit_problem[_token]\"";
    if !tail.contains(name_marker) {
        return None;
    }

    let value_prefix = "value=\"";
    let value_pos = tail.find(value_prefix)?;
    let token_start = id_pos + value_pos + value_prefix.len();
    let token_end = token_start + html[token_start..].find('"')?;
    Some(html[token_start..token_end].to_string())
}

fn submission_error<T>(reason: &str) -> Result<T, Box<TransactionError>> {
    tracing::error!("{reason}");
    Err(Box::new(TransactionError::InvalidMethod {
        method: Method::POST,
    }))
}
