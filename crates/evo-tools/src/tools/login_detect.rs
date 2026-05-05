//! Login page detection heuristics.
//!
//! After `browser_navigate`, the tool checks whether Chrome was redirected to
//! an authentication page.  When detected, the observation includes a
//! `login_required: true` hint so the Skill can decide whether to run login
//! steps or skip straight to the target action.
//!
//! Detection is intentionally lenient — false positives (wrongly reporting
//! login required) are safe; they just trigger an unnecessary login attempt.
//! False negatives (missing a real login page) cause harder downstream failures.

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub(crate) enum PageKind {
    LoginRequired,
    Authenticated,
}

/// Classify the current browser state after navigation.
///
/// `url`       — final URL after any redirects
/// `body_text` — `document.body.innerText` of the loaded page
pub(crate) fn classify(url: &str, body_text: &str) -> PageKind {
    if url_looks_like_login(url) || html_looks_like_login(body_text) {
        PageKind::LoginRequired
    } else {
        PageKind::Authenticated
    }
}

// ---------------------------------------------------------------------------
// URL heuristics
// ---------------------------------------------------------------------------

const LOGIN_URL_PATTERNS: &[&str] = &[
    "/login",
    "/signin",
    "/sign-in",
    "/sign_in",
    "/auth",
    "/authenticate",
    "/sso",
    "/oauth",
    "/accounts/login",
    "/session/new",
    "?redirect",
    "?next=",
    "?returnurl=",
    "?return_to=",
];

fn url_looks_like_login(url: &str) -> bool {
    let lower = url.to_lowercase();
    LOGIN_URL_PATTERNS.iter().any(|pat| lower.contains(pat))
}

// ---------------------------------------------------------------------------
// HTML / visible-text heuristics
// ---------------------------------------------------------------------------

const LOGIN_TEXT_PATTERNS: &[&str] = &[
    "sign in to",
    "log in to",
    "login to",
    "enter your password",
    "forgot password",
    "forgot your password",
    "remember me",
    "create an account",
    "don't have an account",
];

fn html_looks_like_login(body_text: &str) -> bool {
    let lower = body_text.to_lowercase();
    // Require ≥2 matches to avoid false positives from nav-bar "Sign in" links
    // on pages where the user is already authenticated.
    LOGIN_TEXT_PATTERNS
        .iter()
        .filter(|pat| lower.contains(*pat))
        .count()
        >= 2
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_login_urls() {
        for url in [
            "https://accounts.google.com/signin/v2/identifier",
            "https://example.com/login?next=/dashboard",
            "https://bank.com/auth/session/new",
            "https://app.com/sso?return_to=%2Fhome",
        ] {
            assert_eq!(classify(url, ""), PageKind::LoginRequired, "url: {url}");
        }
    }

    #[test]
    fn passes_authenticated_url() {
        assert_eq!(
            classify("https://myaccount.google.com/payments", "Welcome back"),
            PageKind::Authenticated
        );
    }

    #[test]
    fn detects_login_via_body_text() {
        let body = "Sign in to your account\nForgot password? Reset it here.";
        assert_eq!(
            classify("https://example.com/dashboard", body),
            PageKind::LoginRequired
        );
    }

    #[test]
    fn single_signin_link_does_not_trigger() {
        let body = "Welcome to the dashboard. Sign in to another account.";
        assert_eq!(
            classify("https://example.com/dashboard", body),
            PageKind::Authenticated
        );
    }
}
