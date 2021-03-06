use crate::{
    config::{DataType, SinkConfig, SinkContext, SinkDescription},
    sinks::splunk_hec::{self, HecSinkConfig},
    sinks::util::{
        encoding::EncodingConfigWithDefault, BatchConfig, Compression, TowerRequestConfig,
    },
    template::Template,
};
use serde::{Deserialize, Serialize};
use string_cache::DefaultAtom as Atom;

const HOST: &str = "https://cloud.humio.com";

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct HumioLogsConfig {
    token: String,
    // Deprecated name
    #[serde(alias = "host")]
    endpoint: Option<String>,
    source: Option<Template>,
    #[serde(
        skip_serializing_if = "crate::serde::skip_serializing_if_default",
        default
    )]
    encoding: EncodingConfigWithDefault<Encoding>,

    event_type: Option<Template>,

    #[serde(default = "default_host_key")]
    pub host_key: Atom,

    #[serde(default)]
    pub compression: Compression,

    #[serde(default)]
    request: TowerRequestConfig,

    #[serde(default)]
    batch: BatchConfig,
}

fn default_host_key() -> Atom {
    crate::config::LogSchema::default().host_key().clone()
}

inventory::submit! {
    SinkDescription::new_without_default::<HumioLogsConfig>("humio_logs")
}

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq, Clone, Derivative)]
#[serde(rename_all = "snake_case")]
#[derivative(Default)]
pub enum Encoding {
    #[derivative(Default)]
    Json,
    Text,
}

impl From<Encoding> for splunk_hec::Encoding {
    fn from(v: Encoding) -> Self {
        match v {
            Encoding::Json => splunk_hec::Encoding::Json,
            Encoding::Text => splunk_hec::Encoding::Text,
        }
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "humio_logs")]
impl SinkConfig for HumioLogsConfig {
    async fn build(
        &self,
        cx: SinkContext,
    ) -> crate::Result<(super::VectorSink, super::Healthcheck)> {
        self.build_hec_config().build(cx).await
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn sink_type(&self) -> &'static str {
        "humio_logs"
    }
}

impl HumioLogsConfig {
    fn build_hec_config(&self) -> HecSinkConfig {
        let endpoint = self.endpoint.clone().unwrap_or_else(|| HOST.to_string());

        HecSinkConfig {
            token: self.token.clone(),
            endpoint,
            source: self.source.clone(),
            sourcetype: self.event_type.clone(),
            encoding: self.encoding.clone().transmute(),
            compression: self.compression,
            batch: self.batch,
            request: self.request,
            host_key: self.host_key.clone(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::sinks::util::{http::HttpSink, test::load_sink};
    use chrono::Utc;
    use serde::Deserialize;

    #[derive(Deserialize, Debug)]
    struct HecEventJson {
        time: f64,
    }

    #[test]
    fn humio_valid_time_field() {
        let event = Event::from("hello world");

        let (config, _cx) = load_sink::<HumioLogsConfig>(
            r#"
            token = "alsdkfjaslkdfjsalkfj"
            host = "https://127.0.0.1"
        "#,
        )
        .unwrap();
        let config = config.build_hec_config();

        let bytes = config.encode_event(event).unwrap();
        let hec_event = serde_json::from_slice::<HecEventJson>(&bytes[..]).unwrap();

        let now = Utc::now().timestamp_millis() as f64 / 1000f64;
        assert!(
            (hec_event.time - now).abs() < 0.2,
            format!("hec_event.time = {}, now = {}", hec_event.time, now)
        );
        assert_eq!((hec_event.time * 1000f64).fract(), 0f64);
    }
}

#[cfg(test)]
#[cfg(feature = "humio-integration-tests")]
mod integration_tests {
    use super::*;
    use crate::{
        config::{log_schema, SinkConfig, SinkContext},
        sinks::util::Compression,
        test_util::random_string,
        Event,
    };
    use chrono::Utc;
    use futures::{future, stream};
    use serde_json::{json, Value as JsonValue};
    use std::{collections::HashMap, convert::TryFrom};

    // matches humio container address
    const HOST: &str = "http://localhost:8080";

    #[tokio::test]
    async fn humio_insert_message() {
        let cx = SinkContext::new_test();

        let repo = create_repository().await;

        let config = config(&repo.default_ingest_token);

        let (sink, _) = config.build(cx).await.unwrap();

        let message = random_string(100);
        let host = "192.168.1.1".to_string();
        let mut event = Event::from(message.clone());
        let log = event.as_mut_log();
        log.insert(log_schema().host_key(), host.clone());

        sink.run(stream::once(future::ready(event))).await.unwrap();

        let entry = find_entry(repo.name.as_str(), message.as_str()).await;

        assert_eq!(
            message,
            entry
                .fields
                .get("message")
                .expect("no message key")
                .as_str()
                .unwrap()
        );
        assert!(
            entry.error.is_none(),
            "Humio encountered an error parsing this message: {}",
            entry
                .error_msg
                .unwrap_or_else(|| "no error message".to_string())
        );
        assert_eq!(Some(host), entry.host);
    }

    #[tokio::test]
    async fn humio_insert_source() {
        let cx = SinkContext::new_test();

        let repo = create_repository().await;

        let mut config = config(&repo.default_ingest_token);
        config.source = Template::try_from("/var/log/syslog".to_string()).ok();

        let (sink, _) = config.build(cx).await.unwrap();

        let message = random_string(100);
        let event = Event::from(message.clone());
        sink.run(stream::once(future::ready(event))).await.unwrap();

        let entry = find_entry(repo.name.as_str(), message.as_str()).await;

        assert_eq!(entry.source, Some("/var/log/syslog".to_owned()));
        assert!(
            entry.error.is_none(),
            "Humio encountered an error parsing this message: {}",
            entry
                .error_msg
                .unwrap_or_else(|| "no error message".to_string())
        );
    }

    #[tokio::test]
    async fn humio_type() {
        let repo = create_repository().await;

        // sets type
        {
            let mut config = config(&repo.default_ingest_token);
            config.event_type = Template::try_from("json".to_string()).ok();

            let (sink, _) = config.build(SinkContext::new_test()).await.unwrap();

            let message = random_string(100);
            let mut event = Event::from(message.clone());
            // Humio expects to find an @timestamp field for JSON lines
            // https://docs.humio.com/ingesting-data/parsers/built-in-parsers/#json
            event
                .as_mut_log()
                .insert("@timestamp", Utc::now().to_rfc3339());

            sink.run(stream::once(future::ready(event))).await.unwrap();

            let entry = find_entry(repo.name.as_str(), message.as_str()).await;

            assert_eq!(entry.humio_type, "json");
            assert!(
                entry.error.is_none(),
                "Humio encountered an error parsing this message: {}",
                entry
                    .error_msg
                    .unwrap_or_else(|| "no error message".to_string())
            );
        }

        // defaults to none
        {
            let config = config(&repo.default_ingest_token);

            let (sink, _) = config.build(SinkContext::new_test()).await.unwrap();

            let message = random_string(100);
            let event = Event::from(message.clone());

            sink.run(stream::once(future::ready(event))).await.unwrap();

            let entry = find_entry(repo.name.as_str(), message.as_str()).await;

            assert_eq!(entry.humio_type, "none");
        }
    }

    /// create a new test config with the given ingest token
    fn config(token: &str) -> super::HumioLogsConfig {
        super::HumioLogsConfig {
            endpoint: Some(HOST.to_string()),
            token: token.to_string(),
            compression: Compression::None,
            encoding: Encoding::Json.into(),
            batch: BatchConfig {
                max_events: Some(1),
                ..Default::default()
            },
            host_key: log_schema().host_key().clone(),
            ..Default::default()
        }
    }

    /// create a new test humio repository to publish to
    async fn create_repository() -> HumioRepository {
        let client = reqwest::Client::builder().build().unwrap();

        // https://docs.humio.com/api/graphql/
        let graphql_url = format!("{}/graphql", HOST);

        let name = random_string(50);

        let params = json!({
        "query": format!(
            r#"
mutation {{
  createRepository(name:"{}") {{
    repository {{
      name
      type
      ingestTokens {{
        name
        token
      }}
    }}
  }}
}}
"#,
            name
        ),
        });

        let res = client
            .post(&graphql_url)
            .json(&params)
            .send()
            .await
            .unwrap();

        let json: JsonValue = res.json().await.unwrap();
        let repository = &json["data"]["createRepository"]["repository"];

        let token = repository["ingestTokens"].as_array().unwrap()[0]["token"]
            .as_str()
            .unwrap()
            .to_string();

        HumioRepository {
            name: repository["name"].as_str().unwrap().to_string(),
            default_ingest_token: token,
        }
    }

    /// fetch event from the repository that has a matching message value
    async fn find_entry(repository_name: &str, message: &str) -> HumioLog {
        let client = reqwest::Client::builder().build().unwrap();

        // https://docs.humio.com/api/using-the-search-api-with-humio
        let search_url = format!("{}/api/v1/repositories/{}/query", HOST, repository_name);
        let search_query = format!(r#"message="{}""#, message);

        // events are not available to search API immediately
        // poll up 20 times for event to show up
        for _ in 0..20usize {
            let res = client
                .post(&search_url)
                .json(&json!({
                    "queryString": search_query,
                }))
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .unwrap();

            let logs: Vec<HumioLog> = res.json().await.unwrap();

            if !logs.is_empty() {
                return logs[0].clone();
            }
        }
        panic!(
            "did not find event in Humio repository {} with message {}",
            repository_name, message
        );
    }

    #[derive(Debug)]
    struct HumioRepository {
        name: String,
        default_ingest_token: String,
    }

    #[derive(Clone, Deserialize)]
    struct HumioLog {
        #[serde(rename = "#repo")]
        humio_repo: String,

        #[serde(rename = "#type")]
        humio_type: String,

        #[serde(rename = "@error")]
        error: Option<String>,

        #[serde(rename = "@error_msg")]
        error_msg: Option<String>,

        #[serde(rename = "@rawstring")]
        rawstring: String,

        #[serde(rename = "@id")]
        id: String,

        #[serde(rename = "@timestamp")]
        timestamp_millis: u64,

        #[serde(rename = "@timezone")]
        timezone: String,

        #[serde(rename = "@source")]
        source: Option<String>,

        #[serde(rename = "@host")]
        host: Option<String>,

        // fields parsed from ingested log
        #[serde(flatten)]
        fields: HashMap<String, JsonValue>,
    }
}
