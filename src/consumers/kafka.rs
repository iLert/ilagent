use log::{debug, error, info, warn};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::{DaemonContext, hbt, poll};
use rdkafka::client::ClientContext;
use rdkafka::config::{ClientConfig, RDKafkaLogLevel};
use rdkafka::consumer::stream_consumer::StreamConsumer;
use rdkafka::consumer::{BaseConsumer, CommitMode, Consumer, ConsumerContext, Rebalance};
use rdkafka::error::KafkaResult;
use rdkafka::message::Message;
use rdkafka::topic_partition_list::TopicPartitionList;
use rdkafka::util::get_rdkafka_version;
use serde_json::json;

struct CustomContext;

impl ClientContext for CustomContext {}

impl ConsumerContext for CustomContext {
    fn pre_rebalance(&self, _consumer: &BaseConsumer<CustomContext>, rebalance: &Rebalance) {
        debug!("Pre rebalance {:?}", rebalance);
    }

    fn post_rebalance(&self, _consumer: &BaseConsumer<CustomContext>, rebalance: &Rebalance) {
        debug!("Post rebalance {:?}", rebalance);
    }

    fn commit_callback(&self, result: KafkaResult<()>, _offsets: &TopicPartitionList) {
        debug!("Committing offsets: {:?}", result);
    }
}

type LoggingConsumer = StreamConsumer<CustomContext>;

pub async fn run_kafka_job(daemon_ctx: Arc<DaemonContext>) -> () {
    let (version_n, version_s) = get_rdkafka_version();
    info!("rd_kafka_version: 0x{:08x}, {}", version_n, version_s);

    let event_topic = if let Some(topic) = daemon_ctx.config.clone().event_topic {
        topic.clone()
    } else {
        "".to_string()
    };

    let heartbeat_topic = if let Some(topic) = daemon_ctx.config.clone().heartbeat_topic {
        topic.clone()
    } else {
        "".to_string()
    };

    let policy_topic = daemon_ctx.config.clone().policy_topic.unwrap_or_default();

    let mut topics = Vec::new();
    if !event_topic.is_empty() {
        topics.push(event_topic.as_str());
    }
    if !heartbeat_topic.is_empty() {
        topics.push(heartbeat_topic.as_str());
    }
    if !policy_topic.is_empty() {
        topics.push(policy_topic.as_str());
    }

    let brokers = daemon_ctx.config.clone().kafka_brokers.expect("no broker");
    let group_id = daemon_ctx
        .config
        .clone()
        .kafka_group_id
        .expect("no group id");

    let context = CustomContext;
    let consumer: LoggingConsumer = match ClientConfig::new()
        .set("group.id", group_id)
        .set("bootstrap.servers", brokers)
        .set("enable.partition.eof", "false")
        .set("session.timeout.ms", "6000")
        .set("enable.auto.commit", "false")
        .set_log_level(RDKafkaLogLevel::Debug)
        .create_with_context(context)
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Consumer creation failed: {}", e);
            error!("{}", msg);
            if let Some(ref probe) = daemon_ctx.kafka_probe {
                probe.record_error(msg);
                probe.worker_exited.store(true, Ordering::Relaxed);
            }
            return;
        }
    };

    if let Some(ref probe) = daemon_ctx.kafka_probe {
        probe.consumer_started.store(true, Ordering::Relaxed);
    }

    if let Err(e) = consumer.subscribe(&topics) {
        let msg = format!("Can't subscribe to specified topics: {}", e);
        error!("{}", msg);
        if let Some(ref probe) = daemon_ctx.kafka_probe {
            probe.record_error(msg);
            probe.worker_exited.store(true, Ordering::Relaxed);
        }
        return;
    }

    if let Some(ref probe) = daemon_ctx.kafka_probe {
        probe.subscribed.store(true, Ordering::Relaxed);
    }

    loop {
        if !daemon_ctx.running.load(Ordering::Relaxed) {
            info!("Kafka consumer shutting down");
            break;
        }

        let m = tokio::select! {
            result = consumer.recv() => {
                match result {
                    Err(e) => {
                        warn!("Kafka error: {}", e);
                        if let Some(ref probe) = daemon_ctx.kafka_probe {
                            probe.record_error(format!("{}", e));
                        }
                        continue;
                    }
                    Ok(m) => m,
                }
            }
            _ = shutdown_signal(&daemon_ctx) => {
                info!("Kafka consumer received shutdown signal");
                break;
            }
        };

        let payload = match m.payload_view::<str>() {
            None => "",
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                warn!("Error while deserializing message payload: {:?}", e);
                ""
            }
        };

        let message_key = match m.key_view::<str>() {
            None => "",
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                warn!("Error while deserializing message key: {:?}", e);
                ""
            }
        };

        debug!(
            "key: '{:?}', payload: '{}', topic: {}, partition: {}, offset: {}, timestamp: {:?}",
            m.key(),
            payload,
            m.topic(),
            m.partition(),
            m.offset(),
            m.timestamp()
        );

        let should_retry: bool = if m.topic().eq(event_topic.as_str()) {
            handle_event_message(daemon_ctx.clone(), message_key, payload, m.topic()).await
        } else if m.topic().eq(heartbeat_topic.as_str()) {
            handle_heartbeat_message(daemon_ctx.clone(), message_key, payload).await
        } else if !policy_topic.is_empty() && m.topic().eq(policy_topic.as_str()) {
            handle_policy_message(daemon_ctx.clone(), payload).await
        } else {
            warn!(
                "Received Kafka message from unsubscribed topic: {}",
                m.topic()
            );
            false
        };

        if !should_retry {
            consumer
                .commit_message(&m, CommitMode::Async)
                .expect("failed to commit event message");
        } else {
            error!(
                "failed to produce event, did not commit Kafka message offset, will exit in 5 seconds..."
            );
            tokio::time::sleep(Duration::from_millis(5000)).await;
            panic!("failed to produce event, did not commit Kafka message offset");
        }
    }

    if let Some(ref probe) = daemon_ctx.kafka_probe {
        probe.worker_exited.store(true, Ordering::Relaxed);
    }
}

async fn shutdown_signal(daemon_ctx: &Arc<DaemonContext>) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !daemon_ctx.running.load(Ordering::Relaxed) {
            return;
        }
    }
}

async fn handle_policy_message(daemon_context: Arc<DaemonContext>, payload: &str) -> bool {
    super::policy::handle_policy_update(
        &daemon_context.ilert_client,
        &daemon_context.config,
        payload,
    )
    .await
}

async fn handle_heartbeat_message(
    daemon_context: Arc<DaemonContext>,
    _key: &str,
    payload: &str,
) -> bool {
    let parsed = crate::models::heartbeat::HeartbeatJson::parse_heartbeat_json(payload);
    if let Some(heartbeat) = parsed {
        if hbt::ping_heartbeat(
            &daemon_context.ilert_client,
            heartbeat.integrationKey.as_str(),
        )
        .await
        {
            info!(
                "Heartbeat {} pinged, triggered by kafka message",
                heartbeat.integrationKey.as_str()
            );
        }
    }

    false
}

async fn handle_event_message(
    daemon_context: Arc<DaemonContext>,
    key: &str,
    payload: &str,
    topic: &str,
) -> bool {
    let default_details = json!({"messageKey": key, "topic": topic});
    let parsed =
        super::prepare_consumer_event(&daemon_context.config, payload, topic, default_details);
    if let Some(event) = parsed {
        let event_api_path = super::build_event_api_path("kafka", &event.integrationKey);
        let db_event_format =
            crate::models::event::EventQueueItemJson::to_db(event, Some(event_api_path));
        let should_retry =
            poll::send_queued_event(&daemon_context.ilert_client, &db_event_format).await;
        should_retry
    } else {
        false
    }
}
