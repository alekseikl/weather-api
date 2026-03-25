use anyhow::anyhow;
use async_rs::Runtime;
use lapin::{Connection, ConnectionProperties};
use notification_handler::NotificationHandler;
use rpc_client::RpcClient;
use sqlx::postgres::PgPoolOptions;
use tokio::{signal, sync::broadcast, time};
use tracing::{error, info};

use crate::api::ApiServer;

mod api;
mod notification_handler;
mod rpc_client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect("postgres://try-loco:try-loco-pwd@localhost/weather")
        .await?;

    let amqp_conn = Connection::connect_with_runtime(
        "amqp://127.0.0.1:5672",
        ConnectionProperties::default()
            .enable_auto_recover()
            .with_connection_name("weather-api".into()),
        Runtime::tokio_current().clone(),
    )
    .await?;

    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let rpc_client = RpcClient::new(&amqp_conn).await?;
    let notifications = NotificationHandler::new(&amqp_conn).await?;
    let api_server = ApiServer::new(rpc_client.clone(), pool.clone()).await?;

    let rpc_handle = tokio::spawn({
        let rpc_client = rpc_client.clone();
        async move { rpc_client.run().await }
    });

    let notifications_handle = tokio::spawn({
        let shutdown_rx = shutdown_tx.subscribe();

        async move { notifications.run(shutdown_rx).await }
    });

    let http_handle = tokio::spawn(async move { api_server.run(shutdown_rx).await });

    tokio::pin!(rpc_handle);
    tokio::pin!(notifications_handle);
    tokio::pin!(http_handle);

    let wait_for_tasks = async {
        tokio::select! {
            Ok(Err(err)) = &mut notifications_handle => err,
            Ok(Err(err)) = &mut rpc_handle => err,
            Ok(Err(err)) = &mut http_handle => err,
            else => anyhow!("Something went wrong")
        }
    };

    tokio::select! {
        error = wait_for_tasks => {
            error!(error = error.to_string(), "Task failure. App terminated.");
            return Err(error);
        }
        _ = shutdown_signal() => {}
    }

    let _ = shutdown_tx.send(());

    info!("Shutting down");

    let wait_for_tasks = async { tokio::join!(notifications_handle, http_handle) };

    tokio::select! {
        _ = wait_for_tasks => {
            info!("Shut down");
            Ok(())
        }
        _ = time::sleep(std::time::Duration::from_secs(10)) => {
            error!("Shutdown timeout reached");
            Err(anyhow!("Shutdown timeout reached"))
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
