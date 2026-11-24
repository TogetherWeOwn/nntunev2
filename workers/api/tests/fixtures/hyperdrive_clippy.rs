use tokio_postgres::{types::Type, Client, GenericClient, Transaction};

#[allow(dead_code)]
async fn query_and_execute(
    client: &Client,
    tx: &Transaction<'_>,
    generic: &impl GenericClient,
) -> Result<(), tokio_postgres::Error> {
    client
        .query_typed_one("SELECT $1::int4", &[(&1i32, Type::INT4)])
        .await?;
    tx.execute_typed("SELECT $1::int4", &[(&1i32, Type::INT4)])
        .await?;
    generic
        .query_typed_opt("SELECT $1::int4", &[(&1i32, Type::INT4)])
        .await?;

    client.query_one("SELECT 1", &[]).await?; // rejected
    tx.execute("SELECT 1", &[]).await?; // rejected
    generic.prepare_typed("SELECT 1", &[]).await?; // rejected
    Ok(())
}
