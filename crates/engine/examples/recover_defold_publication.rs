//! Reconcile one existing publication from its editor receipt; never create or reapply a plan.
use photocraft_engine::{
    defold_project_local::LocalProjectStore,
    defold_project_model::{PublishPhase, PublishRequest, TransactionId},
    defold_project_store::ProjectStore,
};
use std::path::Path;

fn main() {
    if let Err(error) = recover() {
        eprintln!("Publication recovery: {error}");
        std::process::exit(1);
    }
}

fn recover() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let project = args.next().ok_or("Expected a project directory and transaction ID")?;
    let transaction = args.next().ok_or("Expected a transaction ID")?;
    if args.next().is_some() {
        return Err("Expected exactly a project directory and transaction ID".into());
    }
    let transaction_id = TransactionId::parse(&transaction).map_err(|error| error.to_string())?;
    let store = LocalProjectStore::open(Path::new(&project)).map_err(|error| error.to_string())?;
    let context = store.describe().map_err(|error| error.to_string())?.context;
    let request = PublishRequest { context, transaction_id };
    let receipt = store.publish_status(&request).map_err(|error| error.to_string())?.ok_or("Publication does not exist")?;
    println!("{}", serde_json::to_string_pretty(&receipt).map_err(|error| error.to_string())?);
    if receipt.phase != PublishPhase::Committed {
        return Err("Publication was not confirmed; keep the draft and inspect its recovery status".into());
    }
    Ok(())
}
