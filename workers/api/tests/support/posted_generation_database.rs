//! Shared disposable-database guard for the generation and real-admission banks.
//! Refuse unsafe configurations before opening a socket; never display a URL,
//! database error, query or row in a test failure.

use tokio_postgres::{Client, Config, NoTls};

pub(super) fn validate_disposable_database_config(config: &Config) -> Result<(), &'static str> {
    if config.get_dbname() != Some("nntune_ci") {
        return Err("only the explicitly named disposable nntune_ci database is permitted");
    }
    if !config.get_hostaddrs().is_empty() {
        return Err("hostaddr overrides are not permitted for the disposable test service");
    }
    if config.get_hosts().len() != 1 {
        return Err("one disposable test host only");
    }
    match &config.get_hosts()[0] {
        tokio_postgres::config::Host::Tcp(host)
            if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") =>
        {
            Ok(())
        }
        _ => Err("only the loopback disposable TCP test service is permitted"),
    }
}

fn database_config() -> Config {
    let url = std::env::var("NNTUNE_TEST_DATABASE_URL").unwrap_or_else(|_| {
        panic!("NNTUNE_TEST_DATABASE_URL is required; database tests never silently skip")
    });
    let config = url
        .parse::<Config>()
        .unwrap_or_else(|_| panic!("NNTUNE_TEST_DATABASE_URL is not a valid configuration"));
    if let Err(reason) = validate_disposable_database_config(&config) {
        panic!("{reason}");
    }
    config
}

pub(super) trait Quiet<T> {
    fn quiet(self, ctx: &'static str) -> T;
}

impl<T> Quiet<T> for Result<T, tokio_postgres::Error> {
    fn quiet(self, ctx: &'static str) -> T {
        match self {
            Ok(value) => value,
            Err(_) => panic!("{ctx}"),
        }
    }
}

pub(super) async fn connect() -> Client {
    let config = database_config();
    let (client, connection) = config
        .connect(NoTls)
        .await
        .unwrap_or_else(|_| panic!("connect disposable PostgreSQL"));
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(
            "SET statement_timeout = '10s'; SET lock_timeout = '10s'; \
             SET idle_in_transaction_session_timeout = '15s'",
        )
        .await
        .quiet("bound disposable database operations");
    client
}
