use super::PgClient;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_postgres::{Config, NoTls};

// Complete only the driver's startup over an in-memory pipe. Every database
// operation on the returned client fails because its driver is gone.
pub(crate) async fn offline_client() -> PgClient {
    let (client, mut server) = tokio::io::duplex(256);
    let mut config = Config::new();
    config.user("synthetic");
    let (connected, ()) = tokio::join!(config.connect_raw(client, NoTls), async {
        let len = server.read_u32().await.unwrap();
        server
            .read_exact(&mut vec![0; len as usize - 4])
            .await
            .unwrap();
        server
            .write_all(b"R\0\0\0\x08\0\0\0\0Z\0\0\0\x05I")
            .await
            .unwrap();
    });
    let (client, _) = connected.unwrap();
    PgClient::new(client)
}
