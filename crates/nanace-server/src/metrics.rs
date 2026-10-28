//! Prometheus metrics, served on a separate listener (`NANACE_METRICS_BIND`)
//! so `/metrics` is never reachable through the public ingress.
//!
//! - `nanace_http_requests_total{method, route, status}` and
//!   `nanace_http_request_duration_seconds{method, route}`: `route` is the
//!   matched route pattern (`/api/v1/orgs/{org_id}/search`), never the raw
//!   path, so ids do not become label values.
//! - `nanace_embedding_duration_seconds{kind}`: model time for `passage`
//!   (saving pages, reindex) and `query` (search) embeddings.

use std::sync::{Arc, LazyLock};
use std::time::Instant;

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::{Error, HttpResponse};
use async_trait::async_trait;
use nanace_embed::{EmbedError, Embedder};
use prometheus::{Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder};

struct Metrics {
    registry: Registry,
    requests: IntCounterVec,
    request_seconds: HistogramVec,
    embedding_seconds: HistogramVec,
}

static METRICS: LazyLock<Metrics> = LazyLock::new(|| {
    let registry = Registry::new();
    let requests = IntCounterVec::new(
        Opts::new("nanace_http_requests_total", "HTTP requests by route and status"),
        &["method", "route", "status"],
    )
    .expect("valid metric");
    let request_seconds = HistogramVec::new(
        HistogramOpts::new("nanace_http_request_duration_seconds", "HTTP request latency"),
        &["method", "route"],
    )
    .expect("valid metric");
    let embedding_seconds = HistogramVec::new(
        HistogramOpts::new("nanace_embedding_duration_seconds", "Embedding model latency per call")
            .buckets(vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
        &["kind"],
    )
    .expect("valid metric");
    for collector in [
        Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
        Box::new(request_seconds.clone()),
        Box::new(embedding_seconds.clone()),
    ] {
        registry.register(collector).expect("metric registered once");
    }
    Metrics {
        registry,
        requests,
        request_seconds,
        embedding_seconds,
    }
});

/// Middleware recording every request (`App::wrap(from_fn(metrics::track))`).
pub async fn track(
    req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    let started = Instant::now();
    let method = req.method().to_string();
    let response = next.call(req).await?;
    let route = response
        .request()
        .match_pattern()
        .unwrap_or_else(|| "unmatched".to_owned());
    let status = response.status().as_u16().to_string();
    METRICS
        .requests
        .with_label_values(&[method.as_str(), route.as_str(), status.as_str()])
        .inc();
    METRICS
        .request_seconds
        .with_label_values(&[method.as_str(), route.as_str()])
        .observe(started.elapsed().as_secs_f64());
    Ok(response)
}

/// Prometheus text exposition of every metric.
pub fn render() -> String {
    let mut buffer = Vec::new();
    // Encoding into a Vec cannot fail for well-formed metrics.
    let _ = TextEncoder::new().encode(&METRICS.registry.gather(), &mut buffer);
    String::from_utf8(buffer).unwrap_or_default()
}

pub async fn metrics_handler() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4")
        .body(render())
}

/// An embedder that records how long the model takes.
pub struct TimedEmbedder(pub Arc<dyn Embedder>);

#[async_trait]
impl Embedder for TimedEmbedder {
    fn model_id(&self) -> &str {
        self.0.model_id()
    }

    fn dimensions(&self) -> usize {
        self.0.dimensions()
    }

    async fn embed_passages(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        let started = Instant::now();
        let result = self.0.embed_passages(texts).await;
        METRICS
            .embedding_seconds
            .with_label_values(&["passage"])
            .observe(started.elapsed().as_secs_f64());
        result
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let started = Instant::now();
        let result = self.0.embed_query(text).await;
        METRICS
            .embedding_seconds
            .with_label_values(&["query"])
            .observe(started.elapsed().as_secs_f64());
        result
    }
}

#[cfg(test)]
mod tests {
    use actix_web::middleware::from_fn;
    use actix_web::{App, test, web};
    use nanace_embed::HashingEmbedder;

    use super::*;

    #[actix_web::test]
    async fn records_route_patterns_and_embedding_time() {
        let app = test::init_service(
            App::new()
                .wrap(from_fn(track))
                .route("/things/{id}", web::get().to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;
        for id in ["a", "b"] {
            let req = test::TestRequest::get().uri(&format!("/things/{id}")).to_request();
            assert!(test::call_service(&app, req).await.status().is_success());
        }
        let embedder = TimedEmbedder(Arc::new(HashingEmbedder::new(8)));
        embedder.embed_query("hello").await.expect("embeds");

        let text = render();
        assert!(
            text.contains(r#"nanace_http_requests_total{method="GET",route="/things/{id}",status="200"} 2"#),
            "{text}"
        );
        assert!(!text.contains("/things/a"), "raw paths must not become labels");
        assert!(
            text.contains(r#"nanace_embedding_duration_seconds_count{kind="query"} 1"#),
            "{text}"
        );
    }
}
