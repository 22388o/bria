use bdk::LocalUtxo;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use std::collections::HashMap;

use crate::{app::BlockchainConfig, batch_group::*, error::*, payout::*, primitives::*, wallet::*};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessBatchGroupData {
    pub batch_group_id: BatchGroupId,
    pub account_id: AccountId,
    batch_id: BatchId,
}

impl ProcessBatchGroupData {
    pub fn new(batch_group_id: BatchGroupId, account_id: AccountId) -> Self {
        Self {
            batch_group_id,
            account_id,
            batch_id: BatchId::new(),
        }
    }
}

#[instrument(
    name = "job.process_batch_group",
    skip(pool, payouts, wallets, batch_groups),
    err
)]
pub async fn execute(
    pool: sqlx::PgPool,
    payouts: Payouts,
    wallets: Wallets,
    blockchain_cfg: BlockchainConfig,
    batch_groups: BatchGroups,
    data: ProcessBatchGroupData,
) -> Result<ProcessBatchGroupData, BriaError> {
    let BatchGroup { config: bg_cfg, .. } = batch_groups.find_by_id(data.batch_group_id).await?;

    let unbatched_payouts = payouts.list_unbatched(data.batch_group_id).await?;
    let wallet_ids = unbatched_payouts.keys().copied().collect();
    let mut wallets = wallets.find_by_ids(wallet_ids).await?;

    let fee_rate = crate::fee_estimation::MempoolSpaceClient::fee_rate(bg_cfg.tx_priority).await?;

    let builder = PsbtBuilder::new()
        .consolidate_deprecated_keychains(bg_cfg.consolidate_deprecated_keychains)
        .fee_rate(fee_rate)
        .begin_wallets();

    for (wallet_id, payouts) in unbatched_payouts {
        let wallet = wallets.remove(&wallet_id).expect("Wallet not found");
        // wallet_section.add_wallet(wallet);
        // builder -> wallet , wallet -> keychain, keychain -> wallet
    }

    // persist Batch

    Ok(data)
}

async fn collect_deprecated_utxos(
    deprecated_utxos: &mut HashMap<WalletId, Vec<LocalUtxo>>,
    payouts: &Vec<Payout>,
) -> Result<(), BriaError> {
    for payout in payouts {
        let utxos = deprecated_utxos
            .entry(payout.wallet_id)
            .or_insert_with(|| Vec::<LocalUtxo>::new());
        // let mut old_keychain_inputs = HashMap::new();
        //for (_, wallet) in wallets {
        //    for (keychain_id, keychain) in wallet.keychains[1..].iter() {
        //        //
        //    }
        //}
    }

    Ok(())
}
