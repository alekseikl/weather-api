use std::sync::Arc;

use futures_lite::stream::StreamExt;
use lapin::{
    Channel, Connection,
    options::{
        BasicAckOptions, BasicConsumeOptions, ExchangeDeclareOptions, QueueBindOptions,
        QueueDeclareOptions,
    },
    types::FieldTable,
};
use tracing::{error, info};

pub struct NotificationHandler {
    channel: Channel,
    queue_name: String,
}

impl NotificationHandler {
    pub async fn new(connection: &Connection) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
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

        Ok(Arc::new(Self {
            channel,
            queue_name: queue.name().to_string(),
        }))
    }

    pub async fn listen(&self) -> Result<(), Box<dyn std::error::Error>> {
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

        while let Some(delivery) = consumer.next().await {
            match delivery {
                Ok(delivery) => {
                    let payload = String::from_utf8_lossy(&delivery.data);
                    info!(routing_key = %delivery.routing_key, payload = %payload, "Received weather event");
                    delivery.ack(BasicAckOptions::default()).await?;
                }
                Err(e) => {
                    error!(error = %e, "Error receiving message");
                }
            }
        }

        Ok(())
    }
}
