//! A deterministic PostgreSQL protocol peer, not a PostgreSQL execution engine.
//! Schema OIDs and expected binds come from each test, never from WriterDb.

use std::collections::HashMap;
use std::future::Future;
use std::io::{self, Cursor, Read};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_postgres::config::SslMode;
use tokio_postgres::{Client, Config, NoTls};

pub const TEXT: u32 = 25;
pub const INT8: u32 = 20;
pub const INT4: u32 = 23;
pub const UUID: u32 = 2950;
pub const BOOL: u32 = 16;
pub const BYTEA: u32 = 17;

// Only the test harness sends this simple query. Its completion proves all
// earlier backend responses have reached the real client's response channels.
const BARRIER: &str = "SELECT 'wire barrier'";
const WATCHDOG: Duration = Duration::from_secs(5);
pub const DIAGNOSTIC: &str =
    "private_fixture host=db.example.invalid sql=SELECT fixture_payload data=private_fixture_value";

pub async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(WATCHDOG, future)
        .await
        .expect("wire-test watchdog expired (not an adapter deadline result)")
}

#[derive(Clone, Debug)]
pub enum Param {
    Text(&'static str),
    I64(i64),
    I32(i32),
    Uuid(&'static str, [u8; 16]),
}

impl Param {
    fn oid(&self) -> u32 {
        match self {
            Self::Text(_) => TEXT,
            Self::I64(_) => INT8,
            Self::I32(_) => INT4,
            Self::Uuid(_, _) => UUID,
        }
    }

    fn bytes(&self, format: i16) -> Vec<u8> {
        assert!(matches!(format, 0 | 1), "invalid PostgreSQL bind format");
        match (self, format) {
            (Self::Text(value), _) => value.as_bytes().to_vec(),
            (Self::I64(value), 1) => value.to_be_bytes().to_vec(),
            (Self::I32(value), 1) => value.to_be_bytes().to_vec(),
            (Self::I64(value), 0) => value.to_string().into_bytes(),
            (Self::I32(value), 0) => value.to_string().into_bytes(),
            (Self::Uuid(_, bytes), 1) => bytes.to_vec(),
            (Self::Uuid(text, _), 0) => text.as_bytes().to_vec(),
            _ => unreachable!("format was validated"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Column {
    name: &'static str,
    oid: u32,
}

impl Column {
    pub fn new(name: &'static str, oid: u32) -> Self {
        Self { name, oid }
    }
}

#[derive(Clone, Debug)]
pub enum Cell {
    Text(&'static str),
    I64(i64),
    I32(i32),
    Uuid(&'static str, [u8; 16]),
    Null,
    Raw(Vec<u8>),
}

impl Cell {
    fn bytes(&self, format: i16) -> Option<Vec<u8>> {
        match self {
            Self::Text(value) => Some(Param::Text(value).bytes(format)),
            Self::I64(value) => Some(Param::I64(*value).bytes(format)),
            Self::I32(value) => Some(Param::I32(*value).bytes(format)),
            Self::Uuid(text, bytes) => Some(Param::Uuid(text, *bytes).bytes(format)),
            Self::Null => None,
            Self::Raw(bytes) => Some(bytes.clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Prepare,
    Execute,
}

struct Gate {
    stage: Stage,
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

pub struct GateControl {
    pub entered: oneshot::Receiver<()>,
    pub release: oneshot::Sender<()>,
}

enum Reply {
    Success {
        tag: String,
        rows: Vec<Vec<Cell>>,
    },
    Error {
        stage: Stage,
        sqlstate: &'static str,
    },
    Disconnect,
}

pub struct Step {
    sql: &'static str,
    params: Vec<Param>,
    columns: Vec<Column>,
    reply: Reply,
    gate: Option<Gate>,
}

impl Step {
    pub fn execute(sql: &'static str, params: Vec<Param>, affected: u64) -> Self {
        Self {
            sql,
            params,
            columns: vec![],
            reply: Reply::Success {
                tag: format!("INSERT 0 {affected}"),
                rows: vec![],
            },
            gate: None,
        }
    }

    pub fn query(
        sql: &'static str,
        params: Vec<Param>,
        columns: Vec<Column>,
        rows: Vec<Vec<Cell>>,
    ) -> Self {
        Self {
            sql,
            params,
            columns,
            reply: Reply::Success {
                tag: format!("SELECT {}", rows.len()),
                rows,
            },
            gate: None,
        }
    }

    pub fn error(mut self, stage: Stage, sqlstate: &'static str) -> Self {
        self.reply = Reply::Error { stage, sqlstate };
        self
    }

    pub fn disconnect(mut self) -> Self {
        self.reply = Reply::Disconnect;
        self
    }

    pub fn gated(mut self, stage: Stage) -> (Self, GateControl) {
        let (entered, observed) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        self.gate = Some(Gate {
            stage,
            entered,
            release: wait,
        });
        (
            self,
            GateControl {
                entered: observed,
                release,
            },
        )
    }
}

#[derive(Default, Debug)]
pub struct Trace {
    pub prepared: usize,
    pub bound: usize,
    pub completed: usize,
}

pub struct Session {
    pub client: Client,
    expected: usize,
    stop: Option<oneshot::Sender<()>>,
    peer: Option<JoinHandle<Trace>>,
    driver: Option<JoinHandle<()>>,
}

impl Session {
    pub async fn start(steps: Vec<Step>) -> Self {
        let expected = steps.len();
        let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
        let (stop, stopped) = oneshot::channel();
        let peer = tokio::spawn(serve(peer_io, steps, stopped));
        let connected = bounded(
            Config::new()
                .user("wire_fixture")
                .dbname("wire_fixture")
                .ssl_mode(SslMode::Disable)
                .connect_raw(client_io, NoTls),
        )
        .await;
        let (client, connection) = connected.expect("real tokio-postgres startup handshake");
        let driver = tokio::spawn(async move {
            // Disconnect fixtures deliberately make this future return an error.
            let _ = connection.await;
        });
        Self {
            client,
            expected,
            stop: Some(stop),
            peer: Some(peer),
            driver: Some(driver),
        }
    }

    pub async fn barrier(&self) {
        bounded(self.client.simple_query(BARRIER))
            .await
            .expect("drain earlier protocol responses before releasing the adapter future");
    }

    async fn stop(&mut self) -> Trace {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let trace = bounded(self.peer.take().expect("peer handle"))
            .await
            .expect("wire peer assertion failed");
        let driver = self.driver.take().expect("driver handle");
        driver.abort();
        let _ = bounded(driver).await;
        trace
    }

    pub async fn finish(mut self) -> Trace {
        let trace = self.stop().await;
        assert_eq!(
            trace.completed, self.expected,
            "adapter did not traverse every expected wire operation: {trace:?}"
        );
        trace
    }

    pub async fn finish_abandoned_prepare(mut self) {
        let trace = self.stop().await;
        assert_eq!(self.expected, 1);
        assert_eq!(trace.prepared, 1, "deadline must cover real prepare await");
        assert_eq!(trace.bound, 0, "expired prepare must not proceed to Bind");
        assert_eq!(trace.completed, 0);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(peer) = &self.peer {
            peer.abort();
        }
        if let Some(driver) = &self.driver {
            driver.abort();
        }
    }
}

#[derive(Clone)]
struct Prepared {
    sql: String,
    params: Vec<u32>,
    columns: Vec<Column>,
}

struct Portal {
    statement: Prepared,
    formats: Vec<i16>,
}

async fn serve(
    mut io: DuplexStream,
    mut steps: Vec<Step>,
    mut stopped: oneshot::Receiver<()>,
) -> Trace {
    startup(&mut io).await.expect("peer startup I/O");
    let mut trace = Trace::default();
    let mut statements = HashMap::<String, Prepared>::new();
    let mut portals = HashMap::<String, Portal>::new();
    let mut failed_until_sync = false;
    loop {
        let frame = tokio::select! {
            biased;
            frame = read_frame(&mut io) => frame,
            _ = &mut stopped => return trace,
        };
        let Some((kind, body)) = frame.expect("peer frontend I/O") else {
            return trace;
        };
        if failed_until_sync && kind != b'S' {
            continue;
        }
        let mut input = Cursor::new(body.as_slice());
        match kind {
            b'P' => {
                let name = cstring(&mut input);
                let sql = cstring(&mut input);
                let step = steps
                    .get_mut(trace.completed)
                    .expect("unexpected SQL operation");
                assert_eq!(sql, step.sql, "SQL changed or writer operations reordered");
                let count = u16_read(&mut input);
                let declared: Vec<_> = (0..count).map(|_| u32_read(&mut input)).collect();
                let params: Vec<_> = step.params.iter().map(Param::oid).collect();
                assert!(declared.is_empty() || declared.len() == params.len());
                for (declared, inferred) in declared.iter().zip(&params) {
                    assert!(
                        *declared == 0 || declared == inferred,
                        "typed Parse changed the schema-inferred OID: {declared} != {inferred}"
                    );
                }
                exhausted(&input);
                statements.insert(
                    name,
                    Prepared {
                        sql,
                        params,
                        columns: step.columns.clone(),
                    },
                );
                trace.prepared += 1;
                wait_gate(step, Stage::Prepare).await;
                if let Reply::Error {
                    stage: Stage::Prepare,
                    sqlstate,
                } = step.reply
                {
                    error_response(&mut io, sqlstate).await;
                    trace.completed += 1;
                    failed_until_sync = true;
                } else {
                    send(&mut io, b'1', &[]).await;
                }
            }
            b'D' => {
                let target = byte(&mut input);
                let name = cstring(&mut input);
                exhausted(&input);
                match target {
                    b'S' => {
                        let statement = statements.get(&name).expect("Describe statement");
                        let mut params = Vec::new();
                        params.extend((statement.params.len() as u16).to_be_bytes());
                        for oid in &statement.params {
                            params.extend(oid.to_be_bytes());
                        }
                        send(&mut io, b't', &params).await;
                        describe(&mut io, &statement.columns, &[]).await;
                    }
                    b'P' => {
                        let portal = portals.get(&name).expect("Describe portal");
                        describe(&mut io, &portal.statement.columns, &portal.formats).await;
                    }
                    _ => panic!("unsupported Describe target"),
                }
            }
            b'B' => {
                let portal = cstring(&mut input);
                let statement = cstring(&mut input);
                let statement = statements.get(&statement).expect("Bind statement").clone();
                let step = steps.get(trace.completed).expect("unexpected Bind");
                assert_eq!(statement.sql, step.sql, "cached statement/order mismatch");
                let count = u16_read(&mut input);
                let formats: Vec<_> = (0..count).map(|_| i16_read(&mut input)).collect();
                let count = usize::from(u16_read(&mut input));
                assert_eq!(count, step.params.len(), "bind count changed");
                for (index, param) in step.params.iter().enumerate() {
                    assert_eq!(statement.params[index], param.oid());
                    let len = i32_read(&mut input);
                    assert!(len >= 0, "writer parameter unexpectedly NULL");
                    let mut actual = vec![0; usize::try_from(len).expect("bind length")];
                    Read::read_exact(&mut input, &mut actual).expect("bind bytes");
                    assert_eq!(
                        actual,
                        param.bytes(format_at(&formats, index, count)),
                        "wrong bind value/order at ${} (OID {})",
                        index + 1,
                        param.oid()
                    );
                }
                let count = u16_read(&mut input);
                let formats: Vec<_> = (0..count).map(|_| i16_read(&mut input)).collect();
                exhausted(&input);
                portals.insert(portal, Portal { statement, formats });
                trace.bound += 1;
                send(&mut io, b'2', &[]).await;
            }
            b'E' => {
                let portal = cstring(&mut input);
                assert_eq!(
                    u32_read(&mut input),
                    0,
                    "unexpected partial portal execution"
                );
                exhausted(&input);
                let portal = portals.get(&portal).expect("Execute portal");
                let step = steps.get_mut(trace.completed).expect("unexpected Execute");
                assert_eq!(portal.statement.sql, step.sql);
                wait_gate(step, Stage::Execute).await;
                match &step.reply {
                    Reply::Success { tag, rows } => {
                        for row in rows {
                            assert_eq!(row.len(), step.columns.len(), "invalid test fixture arity");
                            let mut body = Vec::new();
                            body.extend((row.len() as u16).to_be_bytes());
                            for (index, cell) in row.iter().enumerate() {
                                let format = format_at(&portal.formats, index, row.len());
                                if let Some(bytes) = cell.bytes(format) {
                                    body.extend((bytes.len() as i32).to_be_bytes());
                                    body.extend(bytes);
                                } else {
                                    body.extend((-1_i32).to_be_bytes());
                                }
                            }
                            send(&mut io, b'D', &body).await;
                        }
                        let mut body = tag.as_bytes().to_vec();
                        body.push(0);
                        send(&mut io, b'C', &body).await;
                    }
                    Reply::Error { stage, sqlstate } => {
                        assert_eq!(*stage, Stage::Execute);
                        error_response(&mut io, sqlstate).await;
                        failed_until_sync = true;
                    }
                    Reply::Disconnect => {
                        trace.completed += 1;
                        return trace;
                    }
                }
                trace.completed += 1;
            }
            b'S' => {
                exhausted(&input);
                failed_until_sync = false;
                send(&mut io, b'Z', b"I").await;
            }
            b'H' => exhausted(&input),
            b'C' => {
                let target = byte(&mut input);
                let name = cstring(&mut input);
                exhausted(&input);
                match target {
                    b'S' => {
                        statements.remove(&name);
                    }
                    b'P' => {
                        portals.remove(&name);
                    }
                    _ => panic!("unsupported Close target"),
                }
                send(&mut io, b'3', &[]).await;
            }
            b'Q' => {
                assert_eq!(
                    cstring(&mut input),
                    BARRIER,
                    "adapter must use typed driver binds"
                );
                exhausted(&input);
                describe(&mut io, &[Column::new("barrier", TEXT)], &[]).await;
                send(&mut io, b'D', &[0, 1, 0, 0, 0, 2, b'o', b'k']).await;
                send(&mut io, b'C', b"SELECT 1\0").await;
                send(&mut io, b'Z', b"I").await;
            }
            b'X' => return trace,
            _ => panic!("unsupported frontend message {}", char::from(kind)),
        }
    }
}

async fn wait_gate(step: &mut Step, stage: Stage) {
    if step.gate.as_ref().is_some_and(|gate| gate.stage == stage) {
        let gate = step.gate.take().expect("controlled gate");
        gate.entered.send(()).expect("test observes protocol gate");
        gate.release.await.expect("test releases protocol gate");
    }
}

async fn startup(io: &mut DuplexStream) -> io::Result<()> {
    let len = io.read_u32().await?;
    assert!((8..=8192).contains(&len), "bounded startup frame");
    let mut startup = vec![0; len as usize - 4];
    AsyncReadExt::read_exact(io, &mut startup).await?;
    assert_eq!(
        &startup[..4],
        &196608_u32.to_be_bytes(),
        "PostgreSQL v3 startup"
    );
    send(io, b'R', &0_i32.to_be_bytes()).await;
    send(io, b'S', b"server_version\x0014.0\0").await;
    send(io, b'S', b"client_encoding\0UTF8\0").await;
    send(io, b'K', &[0; 8]).await;
    send(io, b'Z', b"I").await;
    Ok(())
}

async fn read_frame(io: &mut DuplexStream) -> io::Result<Option<(u8, Vec<u8>)>> {
    let kind = match io.read_u8().await {
        Ok(kind) => kind,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    };
    let len = io.read_u32().await?;
    assert!((4..=1024 * 1024).contains(&len), "bounded frontend frame");
    let mut body = vec![0; len as usize - 4];
    AsyncReadExt::read_exact(io, &mut body).await?;
    Ok(Some((kind, body)))
}

async fn send(io: &mut DuplexStream, kind: u8, body: &[u8]) {
    io.write_u8(kind).await.expect("backend tag");
    io.write_u32(u32::try_from(body.len() + 4).expect("backend frame length"))
        .await
        .expect("backend length");
    io.write_all(body).await.expect("backend body");
    io.flush().await.expect("backend flush");
}

async fn describe(io: &mut DuplexStream, columns: &[Column], formats: &[i16]) {
    if columns.is_empty() {
        send(io, b'n', &[]).await;
        return;
    }
    let mut body = Vec::new();
    body.extend((columns.len() as u16).to_be_bytes());
    for (index, column) in columns.iter().enumerate() {
        body.extend(column.name.as_bytes());
        body.push(0);
        body.extend(0_u32.to_be_bytes()); // No table identity needed by the client.
        body.extend(0_u16.to_be_bytes());
        body.extend(column.oid.to_be_bytes());
        let size: i16 = match column.oid {
            INT8 => 8,
            INT4 => 4,
            UUID => 16,
            BOOL => 1,
            _ => -1,
        };
        body.extend(size.to_be_bytes());
        body.extend((-1_i32).to_be_bytes());
        body.extend(format_at(formats, index, columns.len()).to_be_bytes());
    }
    send(io, b'T', &body).await;
}

async fn error_response(io: &mut DuplexStream, sqlstate: &str) {
    let mut body = Vec::new();
    for (field, value) in [
        (b'S', "ERROR"),
        (b'V', "ERROR"),
        (b'C', sqlstate),
        (b'M', DIAGNOSTIC),
        (b'D', DIAGNOSTIC),
        (b'H', DIAGNOSTIC),
        (b'W', DIAGNOSTIC),
        (b's', "private_fixture_schema"),
        (b't', "private_fixture_table"),
        (b'c', "private_fixture_column"),
    ] {
        body.push(field);
        body.extend(value.as_bytes());
        body.push(0);
    }
    body.push(0);
    send(io, b'E', &body).await;
}

fn format_at(formats: &[i16], index: usize, count: usize) -> i16 {
    let format = match formats.len() {
        0 => 0,
        1 => formats[0],
        len if len == count => formats[index],
        _ => panic!("invalid format-count in protocol fixture"),
    };
    assert!(matches!(format, 0 | 1));
    format
}

fn byte(input: &mut Cursor<&[u8]>) -> u8 {
    let mut buf = [0; 1];
    Read::read_exact(input, &mut buf).expect("protocol byte");
    buf[0]
}

fn cstring(input: &mut Cursor<&[u8]>) -> String {
    let mut bytes = Vec::new();
    loop {
        let byte = byte(input);
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    String::from_utf8(bytes).expect("UTF-8 protocol string")
}

fn u16_read(input: &mut Cursor<&[u8]>) -> u16 {
    let mut buf = [0; 2];
    Read::read_exact(input, &mut buf).expect("protocol u16");
    u16::from_be_bytes(buf)
}

fn i16_read(input: &mut Cursor<&[u8]>) -> i16 {
    u16_read(input) as i16
}

fn u32_read(input: &mut Cursor<&[u8]>) -> u32 {
    let mut buf = [0; 4];
    Read::read_exact(input, &mut buf).expect("protocol u32");
    u32::from_be_bytes(buf)
}

fn i32_read(input: &mut Cursor<&[u8]>) -> i32 {
    u32_read(input) as i32
}

fn exhausted(input: &Cursor<&[u8]>) {
    assert_eq!(
        input.position(),
        input.get_ref().len() as u64,
        "trailing protocol bytes"
    );
}
