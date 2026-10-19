//! Deterministic I/O below the production HTTPS driver. No network calls.
//!
//! Dropping a query/socket deliberately does NOT release its fixture resource.
//! The driver must call the resource's abort or complete graceful close. A race
//! that drops pending I/O therefore cannot satisfy the teardown assertions.

use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    net::IpAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

use nntune_ingest::nzb_fetch::https_transport::{
    AsyncResolver, BodySink, Clock, ConnectRequest, Connection, Connector, ConnectorError,
    DiagnosticSink, DnsQuery,
};

pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Dns,
    Connect,
    Tls,
    Write,
    Headers,
    Body,
    Close,
}

pub const IO_PHASES: [Phase; 6] = [
    Phase::Dns,
    Phase::Connect,
    Phase::Tls,
    Phase::Write,
    Phase::Headers,
    Phase::Body,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Start(usize, Phase, Duration),
    Resolve(usize, String),
    Dial(usize, Vec<IpAddr>, String, String),
    Write(usize, Vec<u8>),
    Read(usize, Phase, usize),
    DnsAbort(usize),
    SocketAbort(usize),
    Released(usize),
    Dropped(usize),
}

#[derive(Clone, Default)]
pub struct Probe(pub Arc<Mutex<Vec<Event>>>);

impl Probe {
    pub fn push(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }

    pub fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }

    pub fn starts(&self) -> Vec<(usize, Phase, Duration)> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Start(id, phase, budget) => Some((id, phase, budget)),
                _ => None,
            })
            .collect()
    }

    pub fn bytes_read(&self, id: usize, phase: Phase) -> usize {
        self.events()
            .iter()
            .filter_map(|e| match e {
                Event::Read(i, p, n) if *i == id && *p == phase => Some(*n),
                _ => None,
            })
            .sum()
    }
}

#[derive(Default)]
struct GateState {
    open: bool,
    waker: Option<Waker>,
    started: bool,
}

#[derive(Clone, Default)]
pub struct Gate(Arc<Mutex<GateState>>);

impl Gate {
    pub fn open() -> Self {
        let gate = Self::default();
        gate.release();
        gate
    }

    pub fn release(&self) {
        let waker = {
            let mut state = self.0.lock().unwrap();
            state.open = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn poll(
        &self,
        cx: &mut Context<'_>,
        probe: &Probe,
        id: usize,
        phase: Phase,
        budget: Duration,
    ) -> Poll<()> {
        let mut state = self.0.lock().unwrap();
        if !state.started {
            state.started = true;
            probe.push(Event::Start(id, phase, budget));
        }
        if state.open {
            Poll::Ready(())
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[derive(Default)]
struct TimeState {
    now: Duration,
    next_id: usize,
    alarms: BTreeMap<usize, (Duration, Option<Waker>)>,
}

#[derive(Clone, Default)]
pub struct ManualClock(Arc<Mutex<TimeState>>);

impl ManualClock {
    // Only the test advances monotonic time, while the production future is
    // pending. No I/O result supplies elapsed time to the fetcher.
    pub fn advance(&self, elapsed: Duration) {
        let wakes = {
            let mut state = self.0.lock().unwrap();
            state.now += elapsed;
            let now = state.now;
            state
                .alarms
                .values_mut()
                .filter_map(
                    |(deadline, waker)| {
                        if *deadline <= now {
                            waker.take()
                        } else {
                            None
                        }
                    },
                )
                .collect::<Vec<_>>()
        };
        for waker in wakes {
            waker.wake();
        }
    }

    pub fn active_alarms(&self) -> usize {
        self.0.lock().unwrap().alarms.len()
    }
}

struct Alarm {
    clock: ManualClock,
    id: usize,
}

impl Future for Alarm {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.clock.0.lock().unwrap();
        let now = state.now;
        let (deadline, waker) = state.alarms.get_mut(&self.id).unwrap();
        if now >= *deadline {
            Poll::Ready(())
        } else {
            *waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Drop for Alarm {
    fn drop(&mut self) {
        self.clock.0.lock().unwrap().alarms.remove(&self.id);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        self.0.lock().unwrap().now
    }

    fn sleep_until(&self, deadline: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        let mut state = self.0.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        state.alarms.insert(id, (deadline, None));
        Box::pin(Alarm {
            clock: self.clone(),
            id,
        })
    }
}

#[derive(Default)]
struct WakeCount(AtomicUsize);

impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub struct Executor(Arc<WakeCount>);

impl Executor {
    pub fn polls<F: Future>(&self, future: Pin<&mut F>) -> Poll<F::Output> {
        let waker = Waker::from(self.0.clone());
        future.poll(&mut Context::from_waker(&waker))
    }

    pub fn pending<F: Future>(&self, future: Pin<&mut F>) {
        assert!(
            self.polls(future).is_pending(),
            "I/O gate must actually suspend the production future"
        );
    }

    pub fn ready<F: Future>(&self, future: Pin<&mut F>) -> F::Output {
        match self.polls(future) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!(
                "deadline/cancellation must settle without awaiting stalled I/O or graceful close"
            ),
        }
    }

    pub fn wakes(&self) -> usize {
        self.0 .0.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
pub struct Hop {
    pub gates: Vec<(Phase, Gate)>,
    pub head: Vec<u8>,
    pub chunks: Vec<Vec<u8>>,
    pub body_gates: Vec<Gate>,
    pub tls_error: Option<ConnectorError>,
    pub connect_error: Option<ConnectorError>,
    pub read_error: Option<ConnectorError>,
    pub live: Arc<Mutex<bool>>,
}

impl Hop {
    pub fn raw(head: &str, chunks: &[&[u8]]) -> Self {
        Self {
            gates: [
                Phase::Connect,
                Phase::Tls,
                Phase::Write,
                Phase::Headers,
                Phase::Body,
                Phase::Close,
            ]
            .into_iter()
            .map(|p| (p, Gate::open()))
            .collect(),
            head: head.as_bytes().to_vec(),
            chunks: chunks.iter().map(|b| b.to_vec()).collect(),
            body_gates: Vec::new(),
            tls_error: None,
            connect_error: None,
            read_error: None,
            live: Arc::default(),
        }
    }

    pub fn ok(body: &'static [u8]) -> Self {
        Self::raw(
            &format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()),
            &[body],
        )
    }

    pub fn redirect(location: &str, chunks: &[&[u8]]) -> Self {
        Self::raw(
            &format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nConnection: close\r\n\r\n"),
            chunks,
        )
    }

    pub fn stall(&mut self, phase: Phase) {
        *self.gates.iter_mut().find(|(p, _)| *p == phase).unwrap() = (phase, Gate::default());
    }

    pub fn gate(&self, phase: Phase) -> Gate {
        self.gates
            .iter()
            .find(|(p, _)| *p == phase)
            .unwrap()
            .1
            .clone()
    }

    pub fn live(&self) -> bool {
        *self.live.lock().unwrap()
    }
}

struct Segment {
    phase: Phase,
    gate: Gate,
    bytes: VecDeque<u8>,
}

struct Socket {
    id: usize,
    hop: Hop,
    probe: Probe,
    segments: VecDeque<Segment>,
}

impl Socket {
    fn gate(&self, cx: &mut Context<'_>, phase: Phase, remaining: Duration) -> Poll<()> {
        assert!(self.hop.live(), "poll after release");
        self.hop
            .gate(phase)
            .poll(cx, &self.probe, self.id, phase, remaining)
    }

    fn release(&self) {
        let mut live = self.hop.live.lock().unwrap();
        if *live {
            *live = false;
            self.probe.push(Event::Released(self.id));
        }
    }
}

impl Connection for Socket {
    fn poll_connected(
        &mut self,
        cx: &mut Context<'_>,
        remaining: Duration,
    ) -> Poll<Result<(), ConnectorError>> {
        if self.gate(cx, Phase::Connect, remaining).is_pending() {
            return Poll::Pending;
        }
        Poll::Ready(self.hop.connect_error.clone().map_or(Ok(()), Err))
    }

    fn poll_tls(
        &mut self,
        cx: &mut Context<'_>,
        remaining: Duration,
    ) -> Poll<Result<(), ConnectorError>> {
        if self.gate(cx, Phase::Tls, remaining).is_pending() {
            return Poll::Pending;
        }
        Poll::Ready(self.hop.tls_error.clone().map_or(Ok(()), Err))
    }

    fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        bytes: &[u8],
        remaining: Duration,
    ) -> Poll<Result<usize, ConnectorError>> {
        if self.gate(cx, Phase::Write, remaining).is_pending() {
            return Poll::Pending;
        }
        // Short writes force the real driver to finish writing the request.
        let n = bytes.len().min(7);
        self.probe.push(Event::Write(self.id, bytes[..n].to_vec()));
        Poll::Ready(Ok(n))
    }

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        out: &mut [u8],
        remaining: Duration,
    ) -> Poll<Result<usize, ConnectorError>> {
        while self.segments.front().is_some_and(|s| s.bytes.is_empty()) {
            self.segments.pop_front();
        }
        assert!(self.hop.live(), "read after release");
        let (phase, gate) = self.segments.front().map_or_else(
            || (Phase::Body, self.hop.gate(Phase::Body)),
            |s| (s.phase, s.gate.clone()),
        );
        if gate
            .poll(cx, &self.probe, self.id, phase, remaining)
            .is_pending()
        {
            return Poll::Pending;
        }
        if let Some(error) = self.hop.read_error.clone() {
            return Poll::Ready(Err(error));
        }
        let Some(segment) = self.segments.front_mut() else {
            return Poll::Ready(Ok(0));
        };
        assert!(!out.is_empty(), "zero-length reads do not prove EOF");
        let n = out.len().min(segment.bytes.len());
        for byte in &mut out[..n] {
            *byte = segment.bytes.pop_front().unwrap();
        }
        self.probe.push(Event::Read(self.id, phase, n));
        Poll::Ready(Ok(n))
    }

    fn poll_close(
        &mut self,
        cx: &mut Context<'_>,
        remaining: Duration,
    ) -> Poll<Result<(), ConnectorError>> {
        if self.gate(cx, Phase::Close, remaining).is_pending() {
            return Poll::Pending;
        }
        self.release();
        Poll::Ready(Ok(()))
    }

    fn abort(&mut self) {
        self.probe.push(Event::SocketAbort(self.id));
        self.release();
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // Deliberately no release: dropping a Rust future/JS wrapper is not a
        // guarantee of raw-socket teardown during an upgraded TLS handshake.
        self.probe.push(Event::Dropped(self.id));
    }
}

pub struct FixtureConnector {
    hops: VecDeque<Hop>,
    next_id: usize,
    probe: Probe,
}

impl FixtureConnector {
    pub fn new(hops: &[Hop], probe: Probe) -> Self {
        Self {
            hops: hops.iter().cloned().collect(),
            next_id: 0,
            probe,
        }
    }
}

impl Connector for FixtureConnector {
    fn start_connect(&mut self, request: ConnectRequest) -> Box<dyn Connection> {
        let id = self.next_id;
        self.next_id += 1;
        let hop = self
            .hops
            .pop_front()
            .expect("unexpected dial/re-resolution/auto-follow");
        assert_eq!(request.port(), 443);
        self.probe.push(Event::Dial(
            id,
            request.addresses().to_vec(),
            request.tls_server_name().to_owned(),
            request.certificate_name().to_owned(),
        ));
        *hop.live.lock().unwrap() = true;
        let mut segments = VecDeque::from([Segment {
            phase: Phase::Headers,
            gate: hop.gate(Phase::Headers),
            bytes: hop.head.clone().into(),
        }]);
        segments.extend(hop.chunks.iter().enumerate().map(|(index, bytes)| {
            Segment {
                phase: Phase::Body,
                gate: hop
                    .body_gates
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| hop.gate(Phase::Body)),
                bytes: bytes.clone().into(),
            }
        }));
        Box::new(Socket {
            id,
            hop,
            probe: self.probe.clone(),
            segments,
        })
    }
}

#[derive(Clone)]
pub struct Answer {
    pub host: &'static str,
    pub addresses: Vec<IpAddr>,
    pub gate: Gate,
    pub live: Arc<Mutex<bool>>,
}

impl Answer {
    pub fn new(host: &'static str, addresses: &[IpAddr]) -> Self {
        Self {
            host,
            addresses: addresses.to_vec(),
            gate: Gate::open(),
            live: Arc::default(),
        }
    }

    pub fn live(&self) -> bool {
        *self.live.lock().unwrap()
    }
}

pub struct FixtureDns {
    answers: VecDeque<Answer>,
    next_id: usize,
    probe: Probe,
}

impl FixtureDns {
    pub fn new(answers: &[Answer], probe: Probe) -> Self {
        Self {
            answers: answers.iter().cloned().collect(),
            next_id: 0,
            probe,
        }
    }
}

struct Query {
    id: usize,
    answer: Answer,
    probe: Probe,
}

impl AsyncResolver for FixtureDns {
    fn start_resolve(&mut self, host: &str) -> Box<dyn DnsQuery> {
        let answer = self.answers.pop_front().expect("unexpected DNS lookup");
        assert_eq!(host, answer.host);
        let id = self.next_id;
        self.next_id += 1;
        *answer.live.lock().unwrap() = true;
        self.probe.push(Event::Resolve(id, host.to_owned()));
        Box::new(Query {
            id,
            answer,
            probe: self.probe.clone(),
        })
    }
}

impl DnsQuery for Query {
    fn poll_answer(
        &mut self,
        cx: &mut Context<'_>,
        remaining: Duration,
    ) -> Poll<Result<Vec<IpAddr>, ConnectorError>> {
        assert!(self.answer.live(), "poll DNS after release");
        if self
            .answer
            .gate
            .poll(cx, &self.probe, self.id, Phase::Dns, remaining)
            .is_pending()
        {
            return Poll::Pending;
        }
        *self.answer.live.lock().unwrap() = false;
        Poll::Ready(Ok(self.answer.addresses.clone()))
    }

    fn abort(&mut self) {
        self.probe.push(Event::DnsAbort(self.id));
        *self.answer.live.lock().unwrap() = false;
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // Like the socket, Drop is not cancellation acknowledgement.
        self.probe.push(Event::Dropped(self.id));
    }
}

#[derive(Default)]
pub struct Sink {
    pub bytes: Vec<u8>,
    pub appends: Vec<Vec<u8>>,
}

impl BodySink for Sink {
    fn append(&mut self, bytes: &[u8]) {
        self.appends.push(bytes.to_vec());
        self.bytes.extend_from_slice(bytes);
    }
}

#[derive(Clone, Default)]
pub struct Diagnostics(pub Arc<Mutex<Vec<(String, String)>>>);

impl DiagnosticSink for Diagnostics {
    fn rejected(&self, host: &str, code: &'static str) {
        self.0
            .lock()
            .unwrap()
            .push((host.to_owned(), code.to_owned()));
    }
}
