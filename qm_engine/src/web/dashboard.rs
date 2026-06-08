//! Embedded dashboard HTML served at `/`.

use axum::response::Html;

const DASHBOARD_HTML: &str = include_str!("../../static/dashboard.html");

pub async fn dashboard_html() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}
