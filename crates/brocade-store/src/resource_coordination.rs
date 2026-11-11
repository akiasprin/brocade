//! One transaction lock for host resources shared by proxy and CVM writers.
use crate::Result;
use sqlx::{Postgres, Transaction};

pub(crate) async fn lock(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    // Acquire before instance, user and participating-node row locks in both products.
    sqlx::query("SELECT pg_advisory_xact_lock(1112687438,65)")
        .execute(&mut **tx)
        .await?;
    Ok(())
}
