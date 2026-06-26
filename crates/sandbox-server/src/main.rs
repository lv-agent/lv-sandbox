//! # sandbox-server
//!
//! 轻量级 Agent 沙箱服务入口。
//!
//! 启动流程：加载配置 → 初始化 SandboxRunner → 启动 HTTP API → graceful shutdown

use std::sync::Arc;

use sandbox_core::sandbox_context::SandboxRunner;
use sandbox_server::api::AppState;
use sandbox_server::config::AppConfig;
use sandbox_server::scheduler::Scheduler;
use sandbox_server::session::SessionManager;

use tracing_subscriber::EnvFilter;

/// 解析配置文件路径。优先级：--config 参数 > SANDBOX_CONFIG 环境变量 > 默认路径
fn resolve_config_path() -> String {
    // 1. CLI 参数: --config <path>
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--config" && i + 1 < args.len() {
            return args[i + 1].clone();
        }
        if let Some(path) = args[i].strip_prefix("--config=") {
            return path.to_string();
        }
        i += 1;
    }

    // 2. 环境变量
    if let Ok(path) = std::env::var("SANDBOX_CONFIG") {
        return path;
    }

    // 3. 默认路径
    "/etc/sandbox-server/config.yaml".to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. 加载配置
    let config_path = resolve_config_path();
    let config = AppConfig::load_from_path(&config_path)?;

    // 2. 初始化日志
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.server.log_level));
    if config.server.log_format == "text" {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    }

    tracing::info!(
        addr = %config.server.listen_addr,
        max_concurrent = config.server.max_concurrent_jobs,
        "sandbox-server starting"
    );

    // 3. 初始化 SandboxRunner
    let sandbox_config = config.to_sandbox_config();
    let mut runner = SandboxRunner::new(&sandbox_config).await?;

    // 注册配置文件中的自定义 profile（覆盖/新增）
    for (name, pc) in &config.profiles {
        // 跳过内置 profile（已由 with_defaults 注册，配置文件中的版本在 build_profile_registry 中已合并）
        if matches!(name.as_str(), "shell" | "python" | "node") {
            // 内置 profile 如果在配置文件中有定义，也需要覆盖
        }
        match pc.to_profile(name, &config.sandbox) {
            Ok(profile) => {
                tracing::info!(profile = %name, "registered profile");
                runner.register_profile(profile);
            }
            Err(e) => tracing::warn!(profile = %name, error = %e, "invalid profile config, skipping"),
        }
    }
    let runner = Arc::new(runner);

    // 4. 初始化 Scheduler + SessionManager（cr-021 审计 logger;cr-026 共享 runner + audit）
    let audit: Arc<sandbox_server::audit::AuditLogger> = if config.server.audit.enabled {
        match sandbox_server::audit::AuditLogger::file(std::path::Path::new(&config.server.audit.path)) {
            Ok(l) => Arc::new(l),
            Err(e) => {
                tracing::warn!(error = %e, "audit logger init failed, using noop");
                Arc::new(sandbox_server::audit::AuditLogger::noop())
            }
        }
    } else {
        Arc::new(sandbox_server::audit::AuditLogger::noop())
    };
    let scheduler = Arc::new(
        Scheduler::new(runner.clone(), config.server.max_concurrent_jobs).with_audit(audit.clone()),
    );
    let sessions = Arc::new(SessionManager::new(runner, audit));

    // 5. 构建 HTTP 路由
    let state = AppState {
        scheduler: scheduler.clone(),
        sessions,
        config_path: std::path::PathBuf::from(&config_path),
        api_key: config.server.api_key.clone(),
    };
    let app = sandbox_server::api::app(state);

    // 6. 启动 HTTP 服务 + graceful shutdown
    let listener = tokio::net::TcpListener::bind(&config.server.listen_addr).await?;
    tracing::info!(addr = %config.server.listen_addr, "HTTP server ready");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("sandbox-server stopped");
    Ok(())
}

/// 监听 SIGTERM / SIGINT → 触发 graceful shutdown
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl-C");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("received Ctrl-C, starting graceful shutdown");
        },
        _ = terminate => {
            tracing::info!("received SIGTERM, starting graceful shutdown");
        },
    }
}
