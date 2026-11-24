// Compiled only into the synthetic ingest and API WASM artifacts.
pub async fn connect() -> tokio_postgres::Client {
    let socket = worker::Socket::builder()
        .connect("127.0.0.1", 5432)
        .unwrap();
    let config: tokio_postgres::Config =
        "user=postgres password=ci-throwaway dbname=nntune_ci sslmode=disable"
            .parse()
            .unwrap();
    let (client, connection) = config
        .connect_raw(socket, tokio_postgres::NoTls)
        .await
        .unwrap();
    wasm_bindgen_futures::spawn_local(async move { connection.await.unwrap() });
    client
        .batch_execute("SET statement_timeout = '5s'; SET lock_timeout = '2s'")
        .await
        .unwrap();
    client
}
