//! Committed account fixtures for tests that need multiple database sessions.
use tokio_postgres::{Client, Config, NoTls};

pub async fn connect() -> Client {
    let config: Config = std::env::var("NNTUNE_TEST_DATABASE_URL")
        .expect("CI disposable database is required; never silently skip")
        .parse()
        .unwrap();
    assert_eq!(config.get_dbname(), Some("nntune_ci"));
    assert!(config.get_hostaddrs().is_empty(), "no address overrides");
    assert!(
        matches!(config.get_hosts(), [tokio_postgres::config::Host::Tcp(host)]
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    );
    let (client, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '3s'; SET search_path = pg_temp")
        .await
        .unwrap();
    client
}

pub struct Accounts {
    pub observer: Client,
    ids: Vec<String>,
}

impl Accounts {
    pub async fn new(count: usize) -> Self {
        let observer = connect().await;
        let rows = observer.query(
            "INSERT INTO public.users (id, email, created_at) SELECT id, id || '@example.invalid', 1 FROM (SELECT gen_random_uuid()::text AS id FROM generate_series(1, $1)) AS generated RETURNING id",
            &[&(count as i32)],
        ).await.unwrap();
        let ids = rows.into_iter().map(|row| row.get(0)).collect();
        Self { observer, ids }
    }

    pub fn id(&self, index: usize) -> &str {
        &self.ids[index]
    }

    pub async fn cleanup(self) {
        self.observer
            .execute("DELETE FROM public.users WHERE id = ANY($1)", &[&self.ids])
            .await
            .unwrap();
    }
}
