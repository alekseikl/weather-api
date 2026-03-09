use async_rs::Runtime;
use lapin::{Connection, ConnectionProperties};
use notification_handler::NotificationHandler;

mod notification_handler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    let handler = NotificationHandler::new(&amqp_conn).await?;
    handler.listen().await?;

    Ok(())
}
