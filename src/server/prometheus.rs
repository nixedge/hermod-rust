//! Prometheus HTTP server for `hermod-tracer`
//!
//! Exposes four routes:
//!
//! | Route | Description |
//! |-------|-------------|
//! | `GET /` | HTML listing of connected nodes; JSON when `Accept: application/json` |
//! | `GET /targets` | [Prometheus HTTP service-discovery](https://prometheus.io/docs/prometheus/latest/http_sd/) JSON |
//! | `GET /metrics` | Aggregate metrics from all connected nodes |
//! | `GET /{slug}` | Per-node metrics in Prometheus text exposition format |
//!
//! The `/{slug}` path is derived from the node's connection address by
//! lowercasing it and replacing non-alphanumeric characters with `-`
//! (e.g. `/tmp/node.sock` → `tmp-node-sock`).
//!
//! Each node has its own [`prometheus::Registry`] populated by the EKG poller.
//! Metrics are served in the standard Prometheus text format.
//!
//! If `metricsNoSuffix` is set in the config, `_total`, `_int`, and `_double`
//! suffixes are stripped from metric names before serving.

use crate::server::config::Endpoint;
use crate::server::node::TracerState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use prometheus::Encoder;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

/// Shared state passed to axum handlers
#[derive(Clone)]
struct PrometheusState {
    tracer_state: Arc<TracerState>,
    extra_labels: Option<HashMap<String, String>>,
    no_suffix: bool,
    endpoint_host: String,
    endpoint_port: u16,
}

/// Run the Prometheus HTTP server
pub async fn run_prometheus_server(
    endpoint: Endpoint,
    state: Arc<TracerState>,
    labels: Option<HashMap<String, String>>,
    no_suffix: bool,
) -> anyhow::Result<()> {
    let ps = PrometheusState {
        tracer_state: state,
        extra_labels: labels,
        no_suffix,
        endpoint_host: endpoint.ep_host.clone(),
        endpoint_port: endpoint.ep_port,
    };

    let app = Router::new()
        .route("/", get(handle_root))
        .route("/targets", get(handle_targets))
        .route("/metrics", get(handle_all_metrics))
        .route("/:slug", get(handle_node_metrics))
        .with_state(ps);

    let addr = endpoint.to_addr();
    info!("Prometheus server listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// GET / — list connected nodes
async fn handle_root(headers: HeaderMap, State(ps): State<PrometheusState>) -> Response {
    let nodes = ps.tracer_state.node_list().await;

    let wants_json = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false);

    if wants_json {
        let list: Vec<Value> = nodes
            .iter()
            .map(|(id, slug)| json!({"id": id, "slug": slug, "metrics_path": format!("/{}", slug)}))
            .collect();
        Json(json!({"nodes": list})).into_response()
    } else {
        let mut html = String::from(
            "<html><head><title>hermod-tracer</title></head><body><h1>Connected nodes</h1><ul>",
        );
        if nodes.is_empty() {
            html.push_str("<li><em>No nodes connected</em></li>");
        }
        for (id, slug) in &nodes {
            html.push_str(&format!(
                "<li><a href=\"/{slug}\">{id}</a></li>",
                slug = slug,
                id = id
            ));
        }
        html.push_str("</ul></body></html>");
        Html(html).into_response()
    }
}

/// GET /targets — Prometheus HTTP service discovery format
async fn handle_targets(State(ps): State<PrometheusState>) -> impl IntoResponse {
    let nodes = ps.tracer_state.node_list().await;

    let targets: Vec<Value> = nodes
        .iter()
        .map(|(id, slug)| {
            let mut labels = HashMap::new();
            labels.insert("__metrics_path__".to_string(), format!("/{}", slug));
            labels.insert("node_name".to_string(), id.clone());
            if let Some(extra) = &ps.extra_labels {
                for (k, v) in extra {
                    labels.insert(k.clone(), v.clone());
                }
            }
            json!({
                "targets": [format!("{}:{}", ps.endpoint_host, ps.endpoint_port)],
                "labels": labels
            })
        })
        .collect();

    Json(targets)
}

/// GET /metrics — aggregate Prometheus metrics from all connected nodes
async fn handle_all_metrics(State(ps): State<PrometheusState>) -> Response {
    let nodes = ps.tracer_state.all_nodes().await;
    let encoder = prometheus::TextEncoder::new();
    let mut output = String::new();

    for node in &nodes {
        let metric_families = node.registry.gather();
        let mut buf = Vec::new();
        if let Err(e) = encoder.encode(&metric_families, &mut buf) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Encode error: {}", e),
            )
                .into_response();
        }
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if ps.no_suffix {
            text = strip_prometheus_suffixes(text);
        }
        output.push_str(&text);
    }

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        output,
    )
        .into_response()
}

/// GET /:slug — per-node Prometheus metrics
async fn handle_node_metrics(
    Path(slug): Path<String>,
    State(ps): State<PrometheusState>,
) -> Response {
    let node = ps.tracer_state.find_by_slug(&slug).await;
    match node {
        None => (
            StatusCode::NOT_FOUND,
            format!("No node with slug '{}'", slug),
        )
            .into_response(),
        Some(n) => {
            let encoder = prometheus::TextEncoder::new();
            let metric_families = n.registry.gather();
            let mut buf = Vec::new();
            if let Err(e) = encoder.encode(&metric_families, &mut buf) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Encode error: {}", e),
                )
                    .into_response();
            }

            let mut text = String::from_utf8_lossy(&buf).into_owned();

            if ps.no_suffix {
                text = strip_prometheus_suffixes(text);
            }

            (
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; version=0.0.4",
                )],
                text,
            )
                .into_response()
        }
    }
}

/// Strip `_total`, `_int`, `_double` suffixes from Prometheus metric names
fn strip_prometheus_suffixes(text: String) -> String {
    text.lines()
        .map(|line| {
            // Only modify lines that look like metric exposition (not comments)
            if line.starts_with('#') {
                line.to_string()
            } else {
                line.replace("_total ", " ")
                    .replace("_total{", "{")
                    .replace("_int ", " ")
                    .replace("_int{", "{")
                    .replace("_double ", " ")
                    .replace("_double{", "{")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        strip_prometheus_suffixes(s.to_string())
    }

    #[test]
    fn strips_total_before_space() {
        assert!(strip("my_metric_total 42\n").contains("my_metric 42"));
        assert!(!strip("my_metric_total 42\n").contains("_total"));
    }

    #[test]
    fn strips_total_before_brace() {
        let out = strip(r#"my_metric_total{job="x"} 1"#);
        assert!(out.contains(r#"my_metric{job="x"}"#));
        assert!(!out.contains("_total"));
    }

    #[test]
    fn strips_int_before_space() {
        assert!(strip("my_metric_int 10\n").contains("my_metric 10"));
    }

    #[test]
    fn strips_int_before_brace() {
        let out = strip(r#"my_metric_int{a="b"} 5"#);
        assert!(out.contains(r#"my_metric{a="b"}"#));
    }

    #[test]
    fn strips_double_before_space() {
        assert!(strip("my_metric_double 3.14\n").contains("my_metric 3.14"));
    }

    #[test]
    fn strips_double_before_brace() {
        let out = strip(r#"my_metric_double{} 1"#);
        assert!(out.contains(r#"my_metric{} 1"#));
    }

    #[test]
    fn comment_lines_are_unchanged() {
        let input = "# HELP my_metric_total Some counter\n# TYPE my_metric_total counter\n";
        let out = strip(input);
        assert!(out.contains("my_metric_total"));
        // Both comment lines should be present
        assert!(out.contains("# HELP"));
        assert!(out.contains("# TYPE"));
    }

    #[test]
    fn line_without_known_suffix_unchanged() {
        let line = "my_metric 42";
        let out = strip(line);
        assert_eq!(out, "my_metric 42");
    }

    #[test]
    fn mixed_lines_only_data_lines_stripped() {
        let input = "# TYPE c_total counter\nc_total 1\n";
        let out = strip(input);
        assert!(out.contains("# TYPE c_total counter")); // comment unchanged
        assert!(out.contains("c 1")); // data line stripped
    }
}
