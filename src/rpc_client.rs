use std::{collections::HashMap, sync::Arc};

use anyhow::bail;
use chrono::{DateTime, Utc};
use lapin::{
    BasicProperties, Channel, Connection,
    message::Delivery,
    options::{BasicConsumeOptions, BasicPublishOptions, QueueDeclareOptions},
    types::FieldTable,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::{Mutex, oneshot};
use tokio_stream::StreamExt;
use tracing::{error, warn};
use uuid::Uuid;

const QUEUE_NAME: &str = "weather-rpc";

#[derive(Deserialize, Serialize, Debug)]
pub struct Point {
    pub lat: f32,
    pub lon: f32,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Location {
    pub id: i32,
    pub name: String,
    pub coords: Point,
    pub alert_threshold: f32,
    pub prev_temperature: Option<f32>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct RpcRequest<'a, T: Serialize> {
    method: &'a str,
    arguments: T,
}

#[derive(Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "kebab-case")]
enum RpcResult {
    Ok(serde_json::Value),
    Error(String),
}

pub struct RpcClient {
    channel: Channel,
    reply_queue: String,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Vec<u8>>>>>,
}

impl RpcClient {
    pub async fn new(connection: &Connection) -> anyhow::Result<Arc<Self>> {
        let channel = connection.create_channel().await?;

        let reply_queue = channel
            .queue_declare(
                "".into(),
                QueueDeclareOptions {
                    exclusive: true,
                    auto_delete: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await?;

        Ok(Arc::new(Self {
            channel,
            reply_queue: reply_queue.name().to_string(),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    async fn process_delivery(&self, delivery: Delivery) -> anyhow::Result<()> {
        let Some(correlation_id) = delivery
            .properties
            .correlation_id()
            .as_ref()
            .map(ToString::to_string)
        else {
            warn!("RPC reply missing correlation_id");
            return Ok(());
        };

        if let Some(tx) = self.pending.lock().await.remove(&correlation_id) {
            let _ = tx.send(delivery.data);
        } else {
            warn!(correlation_id = %correlation_id, "No pending request for correlation_id");
        }

        Ok(())
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut consumer = self
            .channel
            .basic_consume(
                self.reply_queue.clone().into(),
                "rpc-reply-consumer".into(),
                BasicConsumeOptions {
                    no_ack: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await?;

        while let Some(delivery) = consumer.next().await {
            let delivery = match delivery {
                Ok(delivery) => delivery,
                Err(e) => {
                    error!(error = %e, "Error receiving RPC reply");
                    continue;
                }
            };

            if let Err(e) = self.process_delivery(delivery).await {
                error!(error = %e, "Failed to process delivery");
            }
        }

        Ok(())
    }

    async fn call<Args: Serialize, Output: DeserializeOwned>(
        &self,
        method: &str,
        args: Args,
    ) -> anyhow::Result<Output> {
        let request = RpcRequest {
            method,
            arguments: args,
        };
        let payload = serde_json::to_vec(&request)?;
        let correlation_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        self.pending.lock().await.insert(correlation_id.clone(), tx);

        self.channel
            .basic_publish(
                "".into(),
                QUEUE_NAME.into(),
                BasicPublishOptions::default(),
                &payload,
                BasicProperties::default()
                    .with_reply_to(self.reply_queue.as_str().into())
                    .with_correlation_id(correlation_id.clone().into()),
            )
            .await?
            .await?;

        let result = match serde_json::from_slice::<RpcResult>(&rx.await?)? {
            RpcResult::Ok(result) => result,
            RpcResult::Error(message) => bail!("API error: {}", message),
        };

        Ok(serde_json::from_value(result)?)
    }

    pub async fn get_location(&self, location_id: i32) -> anyhow::Result<Location> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Args {
            location_id: i32,
        }

        self.call("get-location", Args { location_id }).await
    }

    pub async fn get_locations(&self) -> anyhow::Result<Vec<Location>> {
        self.call("get-locations", serde_json::json!(null)).await
    }
}
