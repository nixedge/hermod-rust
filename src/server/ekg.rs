//! EKG mini-protocol client (protocol 1)
//!
//! The acceptor side polls the connected forwarder node for EKG metrics and
//! populates the node's Prometheus registry ([`NodeState::registry`]).
//!
//! ## Wire protocol (`ekg-forward` package, `codecEKGForward`)
//!
//! ```text
//! MsgReq(GetAllMetrics)     →  array(2)\[word(0), array(1)\[word(0)\]\]
//! MsgReq(GetUpdatedMetrics) →  array(2)\[word(0), array(1)\[word(2)\]\]
//! MsgResp(metrics)          →  array(2)\[word(1), ResponseMetrics\]
//! MsgDone                   →  array(1)\[word(1)\]
//! ```
//!
//! `MsgDone` and `MsgResp` share tag `word(1)` and are distinguished by
//! the outer array length (1 = Done, 2 = Resp).
//!
//! `get_all = false` requests only metrics that changed since the last poll;
//! `get_all = true` requests the full snapshot.  Controlled by
//! `ekgRequestFull` in the config.
//!
//! `ResponseMetrics` is a Haskell Generic Serialise newtype:
//! `array(2)\[word(0), \[(text, value)...\]\]` where each pair is
//! `array(2)\[text(name), metricvalue\]`.
//!
//! ## Metric value CBOR (`ekg-core` `Value` type, Generic Serialise)
//!
//! ```text
//! Counter(i64)  →  array(2)\[0, i64\]
//! Gauge(i64)    →  array(2)\[1, i64\]
//! Label(String) →  array(2)\[2, text\]
//! ```
//!
//! `Distribution` is intentionally not supported — the Haskell `ekg-forward`
//! library itself does not yet forward distributions.

use crate::server::node::NodeState;
use pallas_codec::minicbor::{self, Decode, Decoder, Encode, Encoder, data::Type};
use pallas_network::multiplexer::{ChannelBuffer, Error};
use prometheus::{GaugeVec, IntCounterVec, Opts, Registry};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Protocol message types
// ---------------------------------------------------------------------------

/// An EKG metric value
#[derive(Debug, Clone)]
pub enum EkgValue {
    /// An integer counter (monotonically increasing)
    Counter(i64),
    /// A gauge (can go up or down)
    Gauge(i64),
    /// A text label
    Label(String),
}

impl Encode<()> for EkgValue {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.array(2)?;
        match self {
            EkgValue::Counter(v) => {
                e.u8(0)?.i64(*v)?;
            }
            EkgValue::Gauge(v) => {
                e.u8(1)?.i64(*v)?;
            }
            EkgValue::Label(s) => {
                e.u8(2)?.str(s)?;
            }
        }
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for EkgValue {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut ()) -> Result<Self, minicbor::decode::Error> {
        d.array()?;
        let tag = d.u8()?;
        match tag {
            0 => Ok(EkgValue::Counter(d.i64()?)),
            1 => Ok(EkgValue::Gauge(d.i64()?)),
            2 => Ok(EkgValue::Label(d.str()?.to_string())),
            _ => {
                d.skip()?;
                Ok(EkgValue::Label(format!("<tag {}>", tag)))
            }
        }
    }
}

/// Messages in the EKG mini-protocol
#[derive(Debug)]
pub enum EkgMessage {
    /// Request metrics: false=updated only, true=all
    Req(bool),
    /// Response with metric map
    Resp(HashMap<String, EkgValue>),
    /// Terminate the session
    Done,
}

impl Encode<()> for EkgMessage {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut (),
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        match self {
            EkgMessage::Req(get_all) => {
                // array(2)[word(0), Request]
                // GetAllMetrics = array(1)[word(0)], GetUpdatedMetrics = array(1)[word(2)]
                e.array(2)?.u8(0)?;
                if *get_all {
                    e.array(1)?.u8(0)?; // GetAllMetrics
                } else {
                    e.array(1)?.u8(2)?; // GetUpdatedMetrics
                }
            }
            EkgMessage::Resp(metrics) => {
                // array(2)[word(1), ResponseMetrics]
                // ResponseMetrics (Generic Serialise newtype): array(2)[word(0), list_of_pairs]
                e.array(2)?.u8(1)?;
                e.array(2)?.u8(0)?;
                e.array(metrics.len() as u64)?;
                for (k, v) in metrics {
                    e.array(2)?;
                    e.str(k)?;
                    v.encode(e, ctx)?;
                }
            }
            EkgMessage::Done => {
                // array(1)[word(1)]
                e.array(1)?.u8(1)?;
            }
        }
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for EkgMessage {
    fn decode(d: &mut Decoder<'b>, ctx: &mut ()) -> Result<Self, minicbor::decode::Error> {
        let arr_len = d.array()?;
        let tag = d.u8()?;
        // MsgDone and MsgResp share tag word(1); disambiguate by array length.
        match (arr_len, tag) {
            // MsgReq: array(2)[word(0), Request]
            (Some(2), 0) => {
                // Request sum type: array(1)[word(N)]
                // 0 = GetAllMetrics, 2 = GetUpdatedMetrics
                d.array()?;
                let req_tag = d.u8()?;
                Ok(EkgMessage::Req(req_tag == 0))
            }
            // MsgDone: array(1)[word(1)]
            (Some(1), 1) => Ok(EkgMessage::Done),
            // MsgResp: array(2)[word(1), ResponseMetrics]
            (Some(2), 1) => {
                // ResponseMetrics (Generic Serialise newtype): array(2)[word(0), list_of_pairs]
                d.array()?;
                d.u8()?; // constructor index 0
                let list_len = d.array()?;
                let mut metrics = HashMap::new();
                let mut count = 0u64;
                loop {
                    match list_len {
                        None => {
                            if d.datatype()? == Type::Break {
                                d.skip()?;
                                break;
                            }
                        }
                        Some(n) => {
                            if count >= n {
                                break;
                            }
                        }
                    }
                    // Each element is array(2)[text(name), metricvalue]
                    d.array()?;
                    let key = d.str()?.to_string();
                    let val = EkgValue::decode(d, ctx)?;
                    metrics.insert(key, val);
                    count += 1;
                }
                Ok(EkgMessage::Resp(metrics))
            }
            _ => Err(minicbor::decode::Error::message(
                "unknown EKG message format",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// EKG poller
// ---------------------------------------------------------------------------

/// Polls EKG metrics from a forwarder and updates the node's Prometheus registry
pub struct EkgPoller {
    channel: ChannelBuffer,
    node_state: Arc<NodeState>,
    request_full: bool,
    gauge_cache: Mutex<HashMap<String, GaugeVec>>,
    /// Gauges with a `value` label dimension, used for EKG Label metrics
    label_gauge_cache: Mutex<HashMap<String, GaugeVec>>,
    counter_cache: Mutex<HashMap<String, IntCounterVec>>,
    counter_values: Mutex<HashMap<String, i64>>,
}

impl EkgPoller {
    /// Create a new EKG poller
    pub fn new(
        channel: pallas_network::multiplexer::AgentChannel,
        node_state: Arc<NodeState>,
        request_full: bool,
    ) -> Self {
        EkgPoller {
            channel: ChannelBuffer::new(channel),
            node_state,
            request_full,
            gauge_cache: Mutex::new(HashMap::new()),
            label_gauge_cache: Mutex::new(HashMap::new()),
            counter_cache: Mutex::new(HashMap::new()),
            counter_values: Mutex::new(HashMap::new()),
        }
    }

    /// Run the polling loop until the remote sends Done or the channel closes
    pub async fn run_poll_loop(&mut self, freq_secs: f64) {
        let interval = Duration::from_secs_f64(freq_secs.max(0.1));
        loop {
            match self.poll_once().await {
                Ok(true) => {
                    debug!("EKG: node {} sent Done", self.node_state.id);
                    return;
                }
                Ok(false) => {}
                Err(e) => {
                    warn!("EKG poll error for {}: {}", self.node_state.id, e);
                    return;
                }
            }
            tokio::time::sleep(interval).await;
        }
    }

    /// Send one MsgReq, receive MsgResp/MsgDone. Returns true if done.
    async fn poll_once(&mut self) -> Result<bool, Error> {
        let req = EkgMessage::Req(self.request_full);
        self.channel.send_msg_chunks(&req).await?;

        let msg: EkgMessage = self.channel.recv_full_msg().await?;
        match msg {
            EkgMessage::Resp(metrics) => {
                self.update_registry(&metrics);
                Ok(false)
            }
            EkgMessage::Done => Ok(true),
            EkgMessage::Req(_) => Err(Error::Decoding("unexpected Req from forwarder".into())),
        }
    }

    fn update_registry(&self, metrics: &HashMap<String, EkgValue>) {
        let registry = &self.node_state.registry;
        for (name, value) in metrics {
            if let Err(e) = update_metric(
                registry,
                name,
                value,
                &self.gauge_cache,
                &self.label_gauge_cache,
                &self.counter_cache,
                &self.counter_values,
            ) {
                debug!(
                    "EKG registry error for {}/{}: {}",
                    self.node_state.id, name, e
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared registry update helper
// ---------------------------------------------------------------------------

/// Update a single metric in the given registry.
/// Exported so the dispatcher's EkgBackend can reuse the same logic.
pub(crate) fn update_metric(
    registry: &Registry,
    name: &str,
    value: &EkgValue,
    gauge_cache: &Mutex<HashMap<String, GaugeVec>>,
    label_gauge_cache: &Mutex<HashMap<String, GaugeVec>>,
    counter_cache: &Mutex<HashMap<String, IntCounterVec>>,
    counter_values: &Mutex<HashMap<String, i64>>,
) -> anyhow::Result<()> {
    match value {
        EkgValue::Counter(v) => {
            let counter = get_or_create_counter(registry, counter_cache, name)?;
            let mut prev_map = counter_values.lock().unwrap();
            let prev = prev_map.entry(name.to_string()).or_insert(0);
            let delta = v.saturating_sub(*prev).max(0) as u64;
            if delta > 0 {
                counter.with_label_values(&[]).inc_by(delta);
            }
            *prev = *v;
        }
        EkgValue::Gauge(v) => {
            let gauge = get_or_create_gauge(registry, gauge_cache, name)?;
            gauge.with_label_values(&[]).set(*v as f64);
        }
        EkgValue::Label(text) => {
            // Expose label text using the Prometheus `{_info}` pattern:
            // a GaugeVec with a `value` label dimension set to the text value
            // and a gauge value of 1.0.
            // e.g. `ekg_some_metric_info{value="RTS v1.0"} 1.0`
            let metric_name = sanitise_name(&format!("{}_info", name));
            let gauge = get_or_create_label_gauge(registry, label_gauge_cache, &metric_name)?;
            gauge.with_label_values(&[text.as_str()]).set(1.0);
        }
    }
    Ok(())
}

fn get_or_create_gauge(
    registry: &Registry,
    cache: &Mutex<HashMap<String, GaugeVec>>,
    name: &str,
) -> anyhow::Result<GaugeVec> {
    let mut lock = cache.lock().unwrap();
    if let Some(g) = lock.get(name) {
        return Ok(g.clone());
    }
    let opts = Opts::new(sanitise_name(name), name.to_string());
    let g =
        GaugeVec::new(opts, &[]).map_err(|e| anyhow::anyhow!("create gauge {}: {}", name, e))?;
    registry
        .register(Box::new(g.clone()))
        .map_err(|e| anyhow::anyhow!("register gauge {}: {}", name, e))?;
    lock.insert(name.to_string(), g.clone());
    Ok(g)
}

fn get_or_create_counter(
    registry: &Registry,
    cache: &Mutex<HashMap<String, IntCounterVec>>,
    name: &str,
) -> anyhow::Result<IntCounterVec> {
    let mut lock = cache.lock().unwrap();
    if let Some(c) = lock.get(name) {
        return Ok(c.clone());
    }
    let opts = Opts::new(sanitise_name(name), name.to_string());
    let c = IntCounterVec::new(opts, &[])
        .map_err(|e| anyhow::anyhow!("create counter {}: {}", name, e))?;
    registry
        .register(Box::new(c.clone()))
        .map_err(|e| anyhow::anyhow!("register counter {}: {}", name, e))?;
    lock.insert(name.to_string(), c.clone());
    Ok(c)
}

fn get_or_create_label_gauge(
    registry: &Registry,
    cache: &Mutex<HashMap<String, GaugeVec>>,
    name: &str,
) -> anyhow::Result<GaugeVec> {
    let mut lock = cache.lock().unwrap();
    if let Some(g) = lock.get(name) {
        return Ok(g.clone());
    }
    let opts = Opts::new(sanitise_name(name), name.to_string());
    let g = GaugeVec::new(opts, &["value"])
        .map_err(|e| anyhow::anyhow!("create label gauge {}: {}", name, e))?;
    registry
        .register(Box::new(g.clone()))
        .map_err(|e| anyhow::anyhow!("register label gauge {}: {}", name, e))?;
    lock.insert(name.to_string(), g.clone());
    Ok(g)
}

fn sanitise_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pallas_codec::minicbor;

    fn encode<T: minicbor::Encode<()>>(value: &T) -> Vec<u8> {
        let mut buf = Vec::new();
        minicbor::encode_with(value, &mut buf, &mut ()).unwrap();
        buf
    }

    fn decode<T: for<'b> minicbor::Decode<'b, ()>>(buf: &[u8]) -> T {
        minicbor::decode_with(buf, &mut ()).unwrap()
    }

    type GaugeCache = Mutex<HashMap<String, GaugeVec>>;
    type CounterCache = Mutex<HashMap<String, IntCounterVec>>;
    type CounterValues = Mutex<HashMap<String, i64>>;

    fn empty_caches() -> (GaugeCache, GaugeCache, CounterCache, CounterValues) {
        (
            Mutex::new(HashMap::new()),
            Mutex::new(HashMap::new()),
            Mutex::new(HashMap::new()),
            Mutex::new(HashMap::new()),
        )
    }

    // --- EkgValue CBOR ---

    #[test]
    fn ekg_value_counter_round_trip() {
        let v = EkgValue::Counter(-42);
        assert!(matches!(
            decode::<EkgValue>(&encode(&v)),
            EkgValue::Counter(-42)
        ));
    }

    #[test]
    fn ekg_value_gauge_round_trip() {
        let v = EkgValue::Gauge(100);
        assert!(matches!(
            decode::<EkgValue>(&encode(&v)),
            EkgValue::Gauge(100)
        ));
    }

    #[test]
    fn ekg_value_label_round_trip() {
        let v = EkgValue::Label("RTS v1.0".to_string());
        match decode::<EkgValue>(&encode(&v)) {
            EkgValue::Label(s) => assert_eq!(s, "RTS v1.0"),
            _ => panic!("wrong variant"),
        }
    }

    // --- EkgMessage CBOR ---

    #[test]
    fn ekg_req_get_all_round_trip() {
        assert!(matches!(
            decode::<EkgMessage>(&encode(&EkgMessage::Req(true))),
            EkgMessage::Req(true)
        ));
    }

    #[test]
    fn ekg_req_get_updated_round_trip() {
        assert!(matches!(
            decode::<EkgMessage>(&encode(&EkgMessage::Req(false))),
            EkgMessage::Req(false)
        ));
    }

    #[test]
    fn ekg_done_round_trip() {
        assert!(matches!(
            decode::<EkgMessage>(&encode(&EkgMessage::Done)),
            EkgMessage::Done
        ));
    }

    #[test]
    fn ekg_resp_round_trip() {
        let mut metrics = HashMap::new();
        metrics.insert("cpu".to_string(), EkgValue::Gauge(75));
        metrics.insert("mem".to_string(), EkgValue::Counter(1024));
        metrics.insert("rts".to_string(), EkgValue::Label("v1".to_string()));
        let msg = EkgMessage::Resp(metrics);
        match decode::<EkgMessage>(&encode(&msg)) {
            EkgMessage::Resp(m) => {
                assert_eq!(m.len(), 3);
                assert!(matches!(m["cpu"], EkgValue::Gauge(75)));
                assert!(matches!(m["mem"], EkgValue::Counter(1024)));
                assert!(matches!(m["rts"], EkgValue::Label(ref s) if s == "v1"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ekg_resp_empty_round_trip() {
        let msg = EkgMessage::Resp(HashMap::new());
        match decode::<EkgMessage>(&encode(&msg)) {
            EkgMessage::Resp(m) => assert!(m.is_empty()),
            _ => panic!("wrong variant"),
        }
    }

    // --- update_metric ---

    #[test]
    fn update_metric_gauge_sets_value() {
        let registry = Registry::new();
        let (gc, lgc, cc, cv) = empty_caches();
        update_metric(
            &registry,
            "my_gauge",
            &EkgValue::Gauge(42),
            &gc,
            &lgc,
            &cc,
            &cv,
        )
        .unwrap();
        let families = registry.gather();
        assert_eq!(families.len(), 1);
        assert_eq!(families[0].get_name(), "my_gauge");
        assert_eq!(families[0].get_metric()[0].get_gauge().get_value(), 42.0);
    }

    #[test]
    fn update_metric_gauge_overwrites() {
        let registry = Registry::new();
        let (gc, lgc, cc, cv) = empty_caches();
        update_metric(&registry, "g", &EkgValue::Gauge(10), &gc, &lgc, &cc, &cv).unwrap();
        update_metric(&registry, "g", &EkgValue::Gauge(99), &gc, &lgc, &cc, &cv).unwrap();
        let families = registry.gather();
        assert_eq!(families[0].get_metric()[0].get_gauge().get_value(), 99.0);
    }

    #[test]
    fn update_metric_counter_accumulates_deltas() {
        let registry = Registry::new();
        let (gc, lgc, cc, cv) = empty_caches();
        update_metric(&registry, "ops", &EkgValue::Counter(5), &gc, &lgc, &cc, &cv).unwrap();
        update_metric(&registry, "ops", &EkgValue::Counter(8), &gc, &lgc, &cc, &cv).unwrap();
        let families = registry.gather();
        let ops = families.iter().find(|f| f.get_name() == "ops").unwrap();
        // First: delta = 5-0 = 5; second: delta = 8-5 = 3; total = 8
        assert_eq!(ops.get_metric()[0].get_counter().get_value(), 8.0);
    }

    #[test]
    fn update_metric_counter_ignores_decreasing_value() {
        // Counter going backwards (e.g. node restart) should not produce negative deltas
        let registry = Registry::new();
        let (gc, lgc, cc, cv) = empty_caches();
        update_metric(&registry, "c", &EkgValue::Counter(10), &gc, &lgc, &cc, &cv).unwrap();
        update_metric(&registry, "c", &EkgValue::Counter(3), &gc, &lgc, &cc, &cv).unwrap();
        let families = registry.gather();
        // Only the first delta (10) was applied; the backwards step was skipped
        assert_eq!(families[0].get_metric()[0].get_counter().get_value(), 10.0);
    }

    #[test]
    fn update_metric_label_creates_info_gauge_with_value_label() {
        let registry = Registry::new();
        let (gc, lgc, cc, cv) = empty_caches();
        update_metric(
            &registry,
            "rts_version",
            &EkgValue::Label("RTS v1.0".to_string()),
            &gc,
            &lgc,
            &cc,
            &cv,
        )
        .unwrap();
        let families = registry.gather();
        let info = families
            .iter()
            .find(|f| f.get_name() == "rts_version_info")
            .expect("rts_version_info metric should exist");
        let metric = &info.get_metric()[0];
        assert_eq!(metric.get_gauge().get_value(), 1.0);
        let labels = metric.get_label();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].get_name(), "value");
        assert_eq!(labels[0].get_value(), "RTS v1.0");
    }

    #[test]
    fn sanitise_name_replaces_dots_and_slashes() {
        assert_eq!(sanitise_name("a.b/c"), "a_b_c");
    }

    #[test]
    fn sanitise_name_keeps_alphanumeric_and_underscore() {
        assert_eq!(sanitise_name("abc_123"), "abc_123");
    }
}
