use crate::rpc_client::RpcClient;
use anyhow::anyhow;
use async_rs::Runtime;
use lapin::{Connection, ConnectionProperties};
use notification_handler::NotificationHandler;
use tracing::{error, info};

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
            .with_connection_name("weather-checker".into()),
        runtime.clone(),
    )
    .await?;

    let notifications = NotificationHandler::new(&amqp_conn).await?;
    let rpc_client = RpcClient::new(&amqp_conn).await?;

    let notifications_handle = tokio::spawn({
        let notifications = notifications.clone();

        async move { notifications.run().await }
    });

    let rpc_handle = tokio::spawn({
        let rpc_client = rpc_client.clone();

        async move { rpc_client.run().await }
    });

    tokio::spawn(async move {
        match rpc_client.get_location(1).await {
            Ok(location) => info!("{:?}", location),
            Err(e) => error!(error = e.to_string()),
        }

        match rpc_client.get_locations().await {
            Ok(locations) => info!("{:?}", locations),
            Err(e) => error!(error = e.to_string()),
        }
    });

    let error = tokio::select! {
        Ok(Err(err)) = notifications_handle => err,
        Ok(Err(err)) = rpc_handle => err,
        else => anyhow!("Something went wrong")
    };

    error!(error = error.to_string());

    Err(error)
}
