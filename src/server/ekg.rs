//! EKG mini-protocol client (protocol 1)
//!
//! The acceptor side polls the connected forwarder node for EKG metrics and
//! populates the node's Prometheus registry ([`NodeState::registry`]).
//!
//! ## Wire protocol (`ekg-forward` package)
//!
//! ```text
//! MsgReq(get_all: bool)  →  array(2)\[0, bool\]
//! MsgResp(metrics)       →  array(2)\[1, map_of_name_value\]
//! MsgDone                →  array(1)\[2\]
//! ```
//!
//! `get_all = false` requests only metrics that changed since the last poll;
//! `get_all = true` requests the full snapshot.  Controlled by
//! `ekgRequestFull` in the config.
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
use pallas_codec::minicbor::{self, data::Type, Decode, Decoder, Encode, Encoder};
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
            EkgValue::Counter(v) => { e.u8(0)?.i64(*v)?; }
            EkgValue::Gauge(v)   => { e.u8(1)?.i64(*v)?; }
            EkgValue::Label(s)   => { e.u8(2)?.str(s)?; }
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
                e.array(2)?.u8(0)?.bool(*get_all)?;
            }
            EkgMessage::Resp(metrics) => {
                e.array(2)?.u8(1)?;
                e.map(metrics.len() as u64)?;
                for (k, v) in metrics {
                    e.str(k)?;
                    v.encode(e, ctx)?;
                }
            }
            EkgMessage::Done => {
                e.array(1)?.u8(2)?;
            }
        }
        Ok(())
    }
}

impl<'b> Decode<'b, ()> for EkgMessage {
    fn decode(d: &mut Decoder<'b>, ctx: &mut ()) -> Result<Self, minicbor::decode::Error> {
        d.array()?;
        let tag = d.u8()?;
        match tag {
            0 => {
                let get_all = d.bool()?;
                Ok(EkgMessage::Req(get_all))
            }
            1 => {
                let mut metrics = HashMap::new();
                // Map may be definite or indefinite
                let map_len = d.map()?;
                let mut count = 0u64;
                loop {
                    if map_len.is_none() {
                        if d.datatype()? == Type::Break {
                            d.skip()?;
                            break;
                        }
                    } else if count >= map_len.unwrap() {
                        break;
                    }
                    let key = d.str()?.to_string();
                    let val = EkgValue::decode(d, ctx)?;
                    metrics.insert(key, val);
                    count += 1;
                }
                Ok(EkgMessage::Resp(metrics))
            }
            2 => Ok(EkgMessage::Done),
            _ => Err(minicbor::decode::Error::message("unknown EKG tag")),
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
                &self.counter_cache,
                &self.counter_values,
            ) {
                debug!("EKG registry error for {}/{}: {}", self.node_state.id, name, e);
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
        EkgValue::Label(_) => {
            // Labels are text; store presence as gauge=1
            let metric_name = format!("{}_label", name);
            let gauge = get_or_create_gauge(registry, gauge_cache, &metric_name)?;
            gauge.with_label_values(&[]).set(1.0);
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
    let g = GaugeVec::new(opts, &[])
        .map_err(|e| anyhow::anyhow!("create gauge {}: {}", name, e))?;
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

fn sanitise_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}
