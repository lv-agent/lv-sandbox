//! cr-039: OpenTelemetry trace 导出(OTLP/HTTP)。
//!
//! 配置 `server.otel_endpoint` 后,tracing span → OTel span → OTLP collector。
//! 默认关(None = 零开销)。

use anyhow::Result;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;

/// 初始化 OTel tracer(OTLP/HTTP),返回 tracer。
///
/// `endpoint` 形如 `"http://collector:4318"`。调用方用
/// `tracing_opentelemetry::layer().with_tracer(tracer)` 构造 layer。
pub fn init_tracer(endpoint: &str) -> Result<opentelemetry_sdk::trace::Tracer> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()?;

    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .build();

    let tracer = provider.tracer("lv-sandbox");

    opentelemetry::global::set_tracer_provider(provider);

    Ok(tracer)
}
