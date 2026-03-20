use anyhow::anyhow;
use async_rs::Runtime;
use lapin::{Connection, ConnectionProperties};
use notification_handler::NotificationHandler;
use rpc_client::RpcClient;
use tracing::error;

use crate::api::ApiServer;

mod api;
mod notification_handler;
mod rpc_client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let runtime = Runtime::tokio_current();

    let amqp_conn = Connection::connect_with_runtime(
        "amqp://127.0.0.1:5672",
        ConnectionProperties::default()
            .enable_auto_recover()
            .with_connection_name("weather-api".into()),
        runtime.clone(),
    )
    .await?;

    let notifications = NotificationHandler::new(&amqp_conn).await?;
    let rpc_client = RpcClient::new(&amqp_conn).await?;
    let api_server = ApiServer::new(rpc_client.clone()).await?;

    let notifications_handle = tokio::spawn({
        let notifications = notifications.clone();
        async move { notifications.run().await }
    });

    let rpc_handle = tokio::spawn({
        let rpc_client = rpc_client.clone();
        async move { rpc_client.run().await }
    });

    let http_handle = tokio::spawn(async move { api_server.run().await });

    let error = tokio::select! {
        Ok(Err(err)) = notifications_handle => err,
        Ok(Err(err)) = rpc_handle => err,
        Ok(Err(err)) = http_handle => err,
        else => anyhow!("Something went wrong")
    };

    error!(error = error.to_string());

    Err(error)
}
