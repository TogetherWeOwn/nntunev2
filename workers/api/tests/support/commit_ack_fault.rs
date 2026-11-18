//! Owned native client behind the hosted disposable COMMIT relay.
use std::error::Error;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::{unix::pipe::Receiver, TcpStream};
use tokio::time::{sleep, timeout};
use tokio_postgres::{config::SslMode, Client, Config, NoTls};

const DATABASE_URL: &str = "NNTUNE_TEST_DATABASE_URL";
const IO_BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Withheld,
    Rejected,
    Unproven,
}

struct RelayChild(Child);

impl Drop for RelayChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub struct CommitAckFault {
    child: RelayChild,
    receipts: Receiver,
}

impl CommitAckFault {
    /// One admitted PostgreSQL connection; pass the Client by value to the
    /// production Store seam. Use a separate, direct connection for observation.
    pub async fn start() -> Result<(Client, Self), Box<dyn Error>> {
        let url = std::env::var(DATABASE_URL)?;
        let mut child = RelayChild(
            Command::new("python3")
                .args([
                    "-I",
                    "-B",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pg_commit_relay.py"),
                    "--native-ci",
                ])
                .env_clear()
                .env("GITHUB_ACTIONS", std::env::var("GITHUB_ACTIONS")?)
                .env("RUNNER_ENVIRONMENT", std::env::var("RUNNER_ENVIRONMENT")?)
                .env(DATABASE_URL, &url)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()?,
        );
        let mut receipts = Receiver::from_owned_fd(child.0.stdout.take().unwrap().into())?;
        let mut port = [0; 2];
        // The relay validates the exact hosted fixture URL before publishing a port.
        timeout(IO_BUDGET, receipts.read_exact(&mut port)).await??;
        let mut config: Config = url
            .parse()
            .map_err(|_| "invalid disposable configuration")?;
        config.ssl_mode(SslMode::Disable);
        let (client, connection) = timeout(IO_BUDGET, async {
            let socket = TcpStream::connect(([127, 0, 0, 1], u16::from_be_bytes(port))).await?;
            Ok::<_, Box<dyn Error>>(config.connect_raw(socket, NoTls).await?)
        })
        .await??;
        // A transport error here is expected on the fault path; finish(), not
        // the driver result or the Store error, identifies the wire outcome.
        tokio::spawn(connection);
        Ok((client, Self { child, receipts }))
    }

    pub async fn finish(mut self) -> Result<CommitOutcome, Box<dyn Error>> {
        timeout(IO_BUDGET, async {
            let mut receipt = [0];
            self.receipts.read_exact(&mut receipt).await?;
            if self.receipts.read(&mut [0]).await? != 0 {
                return Err("unexpected bytes after relay receipt".into());
            }
            let status = loop {
                if let Some(status) = self.child.0.try_wait()? {
                    break status;
                }
                sleep(Duration::from_millis(10)).await;
            };
            if !status.success() {
                return Err("COMMIT relay did not exit successfully".into());
            }
            match receipt[0] {
                b'W' => Ok(CommitOutcome::Withheld),
                b'R' => Ok(CommitOutcome::Rejected),
                b'U' => Ok(CommitOutcome::Unproven),
                _ => Err("unexpected COMMIT relay receipt".into()),
            }
        })
        .await?
    }
}
