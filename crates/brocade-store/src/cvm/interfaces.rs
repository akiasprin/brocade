use super::*;

pub(super) async fn insert_interface(
    tx: &mut Transaction<'_, Postgres>,
    instance: &str,
    nic: &InterfaceSpec,
) -> Result<()> {
    sqlx::query("INSERT INTO cvm_interfaces(id,instance_id,source_id,purpose) VALUES($1,$2,$3,$4)")
        .bind(&nic.id)
        .bind(instance)
        .bind(&nic.connection.source_id)
        .bind(match nic.connection.purpose {
            InterfacePurpose::Contact => "contact",
            InterfacePurpose::Business => "business",
        })
        .execute(&mut **tx)
        .await?;
    Ok(())
}
