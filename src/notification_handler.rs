use lapin::{
    Channel, Connection,
    message::Delivery,
    options::{
        BasicAckOptions, BasicConsumeOptions, ExchangeDeclareOptions, QueueBindOptions,
        QueueDeclareOptions,
    },
    types::FieldTable,
};
use tokio::{select, sync::broadcast};
use tokio_stream::StreamExt;
use tracing::{error, info};

pub struct NotificationHandler {
    channel: Channel,
    queue_name: String,
}

impl NotificationHandler {
    pub async fn new(connection: &Connection) -> anyhow::Result<Self> {
        let channel = connection.create_channel().await?;

        channel
            .exchange_declare(
                "weather.events".into(),
                lapin::ExchangeKind::Topic,
                ExchangeDeclareOptions::default(),
                FieldTable::default(),
            )
            .await?;

        let queue = channel
            .queue_declare(
                "".into(),
                QueueDeclareOptions {
                    exclusive: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await?;

        channel
            .queue_bind(
                queue.name().clone(),
                "weather.events".into(),
                "weather.*.*".into(),
                QueueBindOptions::default(),
                FieldTable::default(),
            )
            .await?;

        Ok(Self {
            channel,
            queue_name: queue.name().to_string(),
        })
    }

    async fn process_delivery(&self, delivery: Delivery) {
        let payload = String::from_utf8_lossy(&delivery.data);
        info!(routing_key = %delivery.routing_key, payload = %payload, "Received weather event");

        if delivery.ack(BasicAckOptions::default()).await.is_err() {
            error!("Failed to ack delivery");
        }
    }

    pub async fn run(self, mut shutdown_rx: broadcast::Receiver<()>) -> anyhow::Result<()> {
        let mut consumer = self
            .channel
            .basic_consume(
                self.queue_name.as_str().into(),
                "weather-api-consumer".into(),
                BasicConsumeOptions::default(),
                FieldTable::default(),
            )
            .await?;

        info!("Listening for messages on weather.events exchange");

        let run_loop = async {
            while let Some(delivery) = consumer.next().await {
                match delivery {
                    Ok(delivery) => {
                        self.process_delivery(delivery).await;
                    }
                    Err(e) => {
                        error!(error = %e, "Error receiving message");
                    }
                }
            }
        };

        select! {
            _ = run_loop => {},
            _ = shutdown_rx.recv() => {}
        };

        Ok(())
    }
}
