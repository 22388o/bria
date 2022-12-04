use bdk::blockchain::{ElectrumBlockchain, GetHeight};
use electrum_client::Client;
use tracing::instrument;

use crate::{app::BlockchainConfig, bdk::pg::Utxos, error::*, ledger::*, primitives::*, wallet::*};

const MIN_CONFIRMATIONS: u32 = 2;

#[instrument(name = "job.sync_wallet", skip(pool, wallets, ledger), err)]
pub async fn execute(
    pool: sqlx::PgPool,
    wallets: Wallets,
    id: WalletId,
    blockchain_cfg: BlockchainConfig,
    ledger: Ledger,
) -> Result<(), BriaError> {
    let wallet = wallets.find_by_id(id).await?;
    for (keychain_id, cfg) in wallet.keychains {
        let keychain_wallet =
            KeychainWallet::new(pool.clone(), blockchain_cfg.network, keychain_id, cfg);
        let blockchain =
            ElectrumBlockchain::from(Client::new(&blockchain_cfg.electrum_url).unwrap());
        let current_height = blockchain.get_height()?;
        let _ = keychain_wallet.sync(blockchain).await;
        let utxos = Utxos::new(keychain_id, pool.clone());
        loop {
            let mut tx = pool.begin().await?;
            if let Ok(Some((pending_id, new_pending_tx))) = utxos.find_new_pending_tx(&mut tx).await
            {
                ledger
                    .incoming_utxo(
                        tx,
                        IncomingUtxoParams {
                            journal_id: wallet.journal_id,
                            recipient_account_id: wallet.ledger_account_id,
                            pending_id,
                            meta: IncomingUtxoMeta {
                                wallet_id: id,
                                keychain_id,
                                outpoint: new_pending_tx.outpoint,
                                txout: new_pending_tx.txout,
                            },
                        },
                    )
                    .await?;
            } else {
                break;
            }
        }

        loop {
            let mut tx = pool.begin().await?;
            if let Ok(Some((settled_id, new_settled_tx, pending_id))) = utxos
                .find_new_settled_tx(&mut tx, current_height - MIN_CONFIRMATIONS)
                .await
            {
                // ledger
                //     .pending_onchain_income(
                //         tx,
                //         PendingOnchainIncomeParams {
                //             journal_id: wallet.journal_id,
                //             recipient_account_id: wallet.ledger_account_id,
                //             pending_id,
                //             meta: PendingOnchainIncomeMeta {
                //                 wallet_id: id,
                //                 keychain_id,
                //                 outpoint: new_pending_tx.outpoint,
                //                 txout: new_pending_tx.txout,
                //             },
                //         },
                //     )
                //     .await?;
            } else {
                break;
            }
        }
    }
    Ok(())
}
