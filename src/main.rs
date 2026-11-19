use anyhow::Context;
use axum::Extension;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use localforce::config::{Cli, Config};
use localforce::{AppState, Scheme, router, tls};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "localforce=info".into()))
        .init();

    let config = Config::load(Cli::parse())?;
    let cert_dir = config.cert_dir.clone();
    let (host, port, tls_port) = (config.host.clone(), config.port, config.tls_port);
    let state = AppState::new(config)?;

    let http =
        tokio::net::TcpListener::bind((host.as_str(), port)).await.with_context(|| format!("binding {host}:{port}"))?;
    tracing::info!("localforce listening on http://{host}:{port}");

    let certs = tls::load_or_create(&cert_dir)?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let tls_config =
        axum_server::tls_rustls::RustlsConfig::from_pem(certs.cert_pem.into_bytes(), certs.key_pem.into_bytes())
            .await?;
    let tls_addr: std::net::SocketAddr = format!("{host}:{tls_port}").parse()?;
    tracing::info!("                    and https://localhost:{tls_port}");
    tracing::info!("certificate:        {}", cert_dir.join("localforce-cert.pem").display());
    tracing::info!("default user:       {}", state.config.users[0].username);

    let https_app = router(state.clone()).layer(Extension(Scheme("https")));
    let https = tokio::spawn(async move {
        axum_server::bind_rustls(tls_addr, tls_config).serve(https_app.into_make_service()).await
    });
    let http_app = router(state).layer(Extension(Scheme("http")));
    tokio::select! {
        r = axum::serve(http, http_app) => r?,
        r = https => r??,
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}
