use std::sync::Arc;
use std::time::Duration;
use log::{debug, error, info, warn};

use rdkafka::client::ClientContext;
use rdkafka::config::{ClientConfig, RDKafkaLogLevel};
use rdkafka::consumer::stream_consumer::StreamConsumer;
use rdkafka::consumer::{CommitMode, Consumer, ConsumerContext, Rebalance};
use rdkafka::error::KafkaResult;
use rdkafka::message::{Message};
use rdkafka::topic_partition_list::TopicPartitionList;
use rdkafka::util::get_rdkafka_version;
use serde_json::json;
use crate::{il_hbt, il_poll, DaemonContext};
use crate::models::event::EventQueueItemJson;

struct CustomContext;

impl ClientContext for CustomContext {}

impl ConsumerContext for CustomContext {
    fn pre_rebalance(&self, rebalance: &Rebalance) {
        debug!("Pre rebalance {:?}", rebalance);
    }

    fn post_rebalance(&self, rebalance: &Rebalance) {
        debug!("Post rebalance {:?}", rebalance);
    }

    fn commit_callback(&self, result: KafkaResult<()>, _offsets: &TopicPartitionList) {
        debug!("Committing offsets: {:?}", result);
    }
}

type LoggingConsumer = StreamConsumer<CustomContext>;

pub async fn run_kafka_job(daemon_context: Arc<DaemonContext>) -> () {

    let (version_n, version_s) = get_rdkafka_version();
    info!("rd_kafka_version: 0x{:08x}, {}", version_n, version_s);

    let context = CustomContext;

    let event_topic = if let Some(topic) = daemon_context.config.clone().event_topic {
        topic.clone()
    } else {
        "".to_string()
    };

    let heartbeat_topic = if let Some(topic) = daemon_context.config.clone().heartbeat_topic {
        topic.clone()
    } else {
        "".to_string()
    };

    let mut topics = Vec::new();
    if !event_topic.is_empty() {
        topics.push(event_topic.as_str());
    }
    if !heartbeat_topic.is_empty() {
        topics.push(heartbeat_topic.as_str());
    }

    let brokers = daemon_context.config.clone().kafka_brokers.expect("no broker");
    let group_id = daemon_context.config.clone().kafka_group_id.expect("no group id");

    let consumer: LoggingConsumer = ClientConfig::new()
        .set("group.id", group_id)
        .set("bootstrap.servers", brokers)
        .set("enable.partition.eof", "false")
        .set("session.timeout.ms", "6000")
        .set("enable.auto.commit", "false")
        //.set("statistics.interval.ms", "30000")
        //.set("auto.offset.reset", "smallest")
        .set_log_level(RDKafkaLogLevel::Debug)
        .create_with_context(context)
        .expect("Consumer creation failed");

    consumer
        .subscribe(&topics)
        .expect("Can't subscribe to specified topics");

    loop {
        match consumer.recv().await {
            Err(e) => warn!("Kafka error: {}", e),
            Ok(m) => {

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

                debug!("key: '{:?}', payload: '{}', topic: {}, partition: {}, offset: {}, timestamp: {:?}",
                      m.key(), payload, m.topic(), m.partition(), m.offset(), m.timestamp());

                /*
                if let Some(headers) = m.headers() {
                    for header in headers.iter() {
                        info!("  Header {:#?}: {:?}", header.key, header.value);
                    }
                } */

                let should_retry: bool = if m.topic().eq(event_topic.as_str()) {
                    handle_event_message(daemon_context.clone(), message_key, payload, m.topic()).await
                } else if m.topic().eq(heartbeat_topic.as_str()) {
                    handle_heartbeat_message(daemon_context.clone(), message_key, payload).await
                } else {
                    warn!("Received Kafka message from unsubscribed topic: {}", m.topic());
                    // will commit these anyway
                    false
                };

                if !should_retry {
                    // no need to buffer through SQLite, we can use Kafka's offset to ensure redelivery
                    consumer.commit_message(&m, CommitMode::Async).expect("failed to commit event message");
                } else {
                    error!("failed to produce event, did not commit Kafka message offset, will exit in 5 seconds...");
                    tokio::time::sleep(Duration::from_millis(5000)).await;
                    panic!("failed to produce event, did not commit Kafka message offset");
                }
            }
        };
    }
}

async fn handle_heartbeat_message(daemon_context: Arc<DaemonContext>, _key: &str, payload: &str) -> bool {
    let parsed = crate::models::heartbeat::HeartbeatJson::parse_heartbeat_json(payload);
    if let Some(heartbeat) = parsed {
        if il_hbt::ping_heartbeat(&daemon_context.ilert_client, heartbeat.apiKey.as_str()).await {
            info!("Heartbeat {} pinged, triggered by mqtt message", heartbeat.apiKey.as_str());
        }
    }

    false
}

async fn handle_event_message(daemon_context: Arc<DaemonContext>, key: &str, payload: &str, topic: &str) -> bool {
    let parsed = EventQueueItemJson::parse_event_json(&daemon_context.config, payload, topic);
    if let Some(mut event) = parsed {
        // info!("Event queue item: {:?}", event);
        event.customDetails = Some(json!({
            "kafka_key": key,
            "kafka_topic": topic
        }));
        let db_event_format = EventQueueItemJson::to_db(event);
        let should_retry = il_poll::process_queued_event(&daemon_context.ilert_client, &db_event_format).await;
        should_retry
    } else {
        false
    }
}