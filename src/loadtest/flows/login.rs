use goose::metrics::GooseRequestMetric;
use goose::prelude::*;
use reqwest::Method;
use reqwest::header::{COOKIE, HeaderMap, LOCATION, SET_COOKIE};
use reqwest::multipart;
use reqwest::redirect::Policy;
use std::collections::BTreeMap;

use super::super::{AssignedUser, mark_login_failed, mark_login_success, wait_for_all_logins};

pub(in crate::loadtest) async fn ensure_logged_in_and_synced(
    user: &mut GooseUser,
    assigned_team: &AssignedUser,
) -> TransactionResult {
    if !assigned_team.login_completed {
        run_login_flow(user, assigned_team).await?;
    }

    if let Err(error) = wait_for_all_logins().await {
        return fail_and_exit_user(
            user,
            &assigned_team.username,
            &format!("login gate failed: {error:#}"),
        );
    }

    Ok(())
}

async fn run_login_flow(user: &mut GooseUser, assigned_team: &AssignedUser) -> TransactionResult {
    let mut login_page = user.get("/login").await?;
    let response = match login_page.response {
        Ok(resp) => resp,
        Err(error) => {
            return fail_and_exit_user(user, &assigned_team.username, &format!("{error}"));
        }
    };

    if !response.status().is_success() {
        return fail_request_and_exit_user(
            user,
            &assigned_team.username,
            "GET /login returned non-success status",
            &mut login_page.request,
            Some(response.headers()),
        );
    }
    let login_page_set_cookie_headers = extract_set_cookie_headers(response.headers());
    tracing::debug!(
        "user {} GET /login Set-Cookie headers: {:?}",
        assigned_team.username,
        login_page_set_cookie_headers
    );
    let login_page_cookies = extract_cookie_pairs(response.headers());

    let page_html = match response.text().await {
        Ok(text) => text,
        Err(error) => {
            return fail_and_exit_user(user, &assigned_team.username, &format!("{error}"));
        }
    };

    let csrf_token = match extract_csrf_token(&page_html) {
        Some(token) => token,
        None => {
            return fail_and_exit_user(
                user,
                &assigned_team.username,
                "missing _csrf_token in /login html",
            );
        }
    };

    let cookie_header = format_cookie_header(login_page_cookies.clone());
    let login_form = multipart::Form::new()
        .text("_username", assigned_team.username.clone())
        .text("_password", assigned_team.password.clone())
        .text("_csrf_token", csrf_token);
    let login_url = user.build_url("/login")?;
    let login_client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|error| Box::new(TransactionError::Reqwest(error)))?;
    let mut login_request = login_client.post(&login_url).multipart(login_form);
    if !cookie_header.is_empty() {
        login_request = login_request.header(COOKIE, cookie_header);
    }
    let response = match login_request.send().await {
        Ok(resp) => resp,
        Err(error) => {
            return fail_and_exit_user(user, &assigned_team.username, &format!("{error}"));
        }
    };
    let response_status = response.status();
    let response_url = response.url().clone();
    let response_headers = response.headers().clone();

    tracing::debug!(
        "user {} POST /login response: status={} url={} headers={:?}",
        assigned_team.username,
        response_status,
        response_url,
        response_headers,
    );

    if !response_status.is_success() && !response_status.is_redirection() {
        return fail_and_exit_user(
            user,
            &assigned_team.username,
            "POST /login returned unexpected status",
        );
    }

    let login_submit_set_cookie_headers = extract_set_cookie_headers(&response_headers);
    tracing::debug!(
        "user {} POST /login Set-Cookie headers: {:?}",
        assigned_team.username,
        login_submit_set_cookie_headers
    );
    let login_submit_cookies = extract_cookie_pairs(&response_headers);
    let mut session_cookie =
        format_cookie_header(merge_cookie_pairs(login_page_cookies, login_submit_cookies));
    if response_status.is_redirection() {
        let location = match response_headers.get(LOCATION).and_then(|value| value.to_str().ok()) {
            Some(value) => value,
            None => {
                return fail_and_exit_user(
                    user,
                    &assigned_team.username,
                    "POST /login returned redirection without Location header",
                );
            }
        };
        let redirect_url = match response_url.join(location) {
            Ok(url) => url.to_string(),
            Err(error) => {
                return fail_and_exit_user(
                    user,
                    &assigned_team.username,
                    &format!("failed to resolve login redirect url: {error}"),
                );
            }
        };

        tracing::debug!(
            "user {} following login redirect with refreshed cookie: {}",
            assigned_team.username,
            redirect_url
        );
        let redirect_builder = user
            .get_request_builder(&GooseMethod::Get, &redirect_url)?
            .header(COOKIE, session_cookie.clone());
        let redirect_request = GooseRequest::builder()
            .method(GooseMethod::Get)
            .path(redirect_url.as_str())
            .set_request_builder(redirect_builder)
            .build();
        let redirect_response = user.request(redirect_request).await?;
        if let Ok(response) = redirect_response.response {
            let redirect_cookies = extract_cookie_pairs(response.headers());
            session_cookie = format_cookie_header(merge_cookie_pairs(
                parse_cookie_header_pairs(&session_cookie),
                redirect_cookies,
            ));
        }
    }
    tracing::debug!(
        "user {} merged login Cookie header: {}",
        assigned_team.username,
        session_cookie
    );
    if session_cookie.is_empty() {
        return fail_and_exit_user(
            user,
            &assigned_team.username,
            "login responses did not contain a usable session cookie",
        );
    }
    if !contains_cookie_name(&session_cookie, "PHPSESSID") {
        return fail_and_exit_user(
            user,
            &assigned_team.username,
            "login responses did not contain PHPSESSID cookie",
        );
    }

    let assigned = match user.get_session_data_mut::<AssignedUser>() {
        Some(data) => data,
        None => {
            return fail_and_exit_user(
                user,
                &assigned_team.username,
                "missing AssignedUser session after login",
            );
        }
    };
    assigned.login_completed = true;
    assigned.session_cookie = Some(session_cookie);

    let logged_in_count = mark_login_success();
    tracing::info!(
        "user {} login complete ({logged_in_count} users ready)",
        assigned_team.username
    );

    Ok(())
}

fn fail_and_exit_user(user: &mut GooseUser, username: &str, reason: &str) -> TransactionResult {
    mark_login_failed();
    tracing::error!("user {username} login failed: {reason}");
    user.config.iterations = user.get_iterations() + 1;
    Err(Box::new(TransactionError::InvalidMethod {
        method: Method::GET,
    }))
}

fn fail_request_and_exit_user(
    user: &mut GooseUser,
    username: &str,
    tag: &str,
    request: &mut GooseRequestMetric,
    headers: Option<&reqwest::header::HeaderMap>,
) -> TransactionResult {
    mark_login_failed();
    tracing::error!("user {username} login failed: {tag}");
    user.config.iterations = user.get_iterations() + 1;
    user.set_failure(tag, request, headers, None)
}

fn extract_cookie_pairs(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|set_cookie| set_cookie.split(';').next())
        .map(str::trim)
        .filter(|cookie_pair| !cookie_pair.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
}

fn extract_set_cookie_headers(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect::<Vec<_>>()
}

fn merge_cookie_pairs(primary: Vec<String>, secondary: Vec<String>) -> Vec<String> {
    let mut cookies = BTreeMap::<String, String>::new();
    for cookie_pair in primary.into_iter().chain(secondary) {
        let Some((name, value)) = cookie_pair.split_once('=') else {
            continue;
        };
        cookies.insert(name.trim().to_string(), value.trim().to_string());
    }
    cookies
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
}

fn format_cookie_header(cookie_pairs: Vec<String>) -> String {
    cookie_pairs.join("; ")
}

fn parse_cookie_header_pairs(cookie_header: &str) -> Vec<String> {
    cookie_header
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
}

fn contains_cookie_name(cookie_header: &str, expected_name: &str) -> bool {
    cookie_header
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .any(|(name, _)| name.trim().eq_ignore_ascii_case(expected_name))
}

fn extract_csrf_token(html: &str) -> Option<String> {
    let csrf_name = "name=\"_csrf_token\"";
    let name_index = html.find(csrf_name)?;
    let html_after_name = &html[name_index..];
    let value_prefix = "value=\"";
    let value_index = html_after_name.find(value_prefix)?;
    let token_start = name_index + value_index + value_prefix.len();
    let token_end = token_start + html[token_start..].find('"')?;
    Some(html[token_start..token_end].to_string())
}
