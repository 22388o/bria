mod effective_allocation;
mod entity;
pub mod error;
mod repo;

use bdk::{wallet::AddressInfo, LocalUtxo};
use sqlx::{Pool, Postgres, Transaction};
use tracing::instrument;

use std::collections::HashMap;

use crate::primitives::{bitcoin::OutPoint, *};
pub use entity::*;
use error::UtxoError;
use repo::*;

#[derive(Clone)]
pub struct Utxos {
    utxos: UtxoRepo,
    pool: Pool<Postgres>,
}

impl Utxos {
    pub fn new(pool: &Pool<Postgres>) -> Self {
        Self {
            utxos: UtxoRepo::new(pool.clone()),
            pool: pool.clone(),
        }
    }

    #[instrument(name = "utxos.new_utxo_detected", skip(self), err)]
    #[allow(clippy::too_many_arguments)]
    pub async fn new_utxo_detected(
        &self,
        account_id: AccountId,
        wallet_id: WalletId,
        keychain_id: KeychainId,
        address: &AddressInfo,
        utxo: &LocalUtxo,
        sats_per_vbyte_when_created: f32,
        self_pay: bool,
    ) -> Result<Option<(LedgerTransactionId, Transaction<'_, Postgres>)>, UtxoError> {
        let new_utxo = NewUtxo::builder()
            .account_id(account_id)
            .wallet_id(wallet_id)
            .keychain_id(keychain_id)
            .outpoint(utxo.outpoint)
            .kind(address.keychain)
            .address_idx(address.index)
            .address(address.to_string())
            .script_hex(format!("{:x}", utxo.txout.script_pubkey))
            .value(utxo.txout.value)
            .bdk_spent(utxo.is_spent)
            .sats_per_vbyte_when_created(sats_per_vbyte_when_created)
            .self_pay(self_pay)
            .build()
            .expect("Could not build NewUtxo");
        let mut tx = self.pool.begin().await?;
        let tx_id = self.utxos.persist_utxo(&mut tx, new_utxo).await?;
        Ok(tx_id.map(|id| (id, tx)))
    }

    #[instrument(name = "utxos.settle_utxo", skip(self, tx), err)]
    pub async fn settle_utxo(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        keychain_id: KeychainId,
        outpoint: OutPoint,
        bdk_spent: bool,
        block_height: u32,
    ) -> Result<SettledUtxo, UtxoError> {
        self.utxos
            .mark_utxo_settled(tx, keychain_id, outpoint, bdk_spent, block_height)
            .await
    }

    #[instrument(name = "utxos.spend_detected", skip(self, inputs), err)]
    #[allow(clippy::type_complexity)]
    #[allow(clippy::too_many_arguments)]
    pub async fn spend_detected(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        account_id: AccountId,
        wallet_id: WalletId,
        keychain_id: KeychainId,
        tx_id: LedgerTransactionId,
        inputs: impl Iterator<Item = &OutPoint>,
        change_utxos: &Vec<(&LocalUtxo, AddressInfo)>,
        sats_per_vbyte: f32,
    ) -> Result<Option<(Satoshis, HashMap<bitcoin::OutPoint, Satoshis>)>, UtxoError> {
        for (utxo, address) in change_utxos.iter() {
            let new_utxo = NewUtxo::builder()
                .account_id(account_id)
                .wallet_id(wallet_id)
                .keychain_id(keychain_id)
                .utxo_detected_ledger_tx_id(tx_id)
                .outpoint(utxo.outpoint)
                .kind(address.keychain)
                .address_idx(address.index)
                .address(address.to_string())
                .script_hex(format!("{:x}", utxo.txout.script_pubkey))
                .value(utxo.txout.value)
                .bdk_spent(utxo.is_spent)
                .sats_per_vbyte_when_created(sats_per_vbyte)
                .self_pay(true)
                .build()
                .expect("Could not build NewUtxo");
            let res = self.utxos.persist_utxo(tx, new_utxo).await?;
            if res.is_none() {
                return Ok(None);
            }
        }
        let utxos = self
            .utxos
            .mark_spent(tx, keychain_id, inputs, tx_id)
            .await?;
        if utxos.is_empty() {
            return Ok(None);
        }
        let (total_settled_in, allocations) =
            effective_allocation::withdraw_from_effective_when_settled(
                utxos,
                change_utxos.iter().fold(Satoshis::ZERO, |s, (u, _)| {
                    s + Satoshis::from(u.txout.value)
                }),
            );
        Ok(Some((total_settled_in, allocations)))
    }

    #[instrument(name = "utxos.spend_settled", skip(self, tx, inputs), err)]
    pub async fn spend_settled(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        keychain_id: KeychainId,
        inputs: impl Iterator<Item = &OutPoint>,
        change_utxo: Option<LocalUtxo>,
        block_height: u32,
    ) -> Result<Option<(LedgerTransactionId, LedgerTransactionId, bool)>, UtxoError> {
        let (spend_tx_id, change_spent) = if let Some(utxo) = change_utxo {
            let settled_utxo = self
                .utxos
                .mark_utxo_settled(tx, keychain_id, utxo.outpoint, utxo.is_spent, block_height)
                .await?;
            (
                settled_utxo.utxo_settled_ledger_tx_id,
                settled_utxo.spend_detected_ledger_tx_id.is_some(),
            )
        } else {
            (LedgerTransactionId::new(), false)
        };
        let pending_spend_tx_id = self
            .utxos
            .settle_utxo(tx, keychain_id, inputs, spend_tx_id)
            .await?;
        Ok(pending_spend_tx_id.map(|id| (id, spend_tx_id, change_spent)))
    }

    #[instrument(name = "utxos.find_keychain_utxos", skip_all, err)]
    pub async fn find_keychain_utxos(
        &self,
        keychain_ids: impl Iterator<Item = KeychainId>,
    ) -> Result<HashMap<KeychainId, KeychainUtxos>, UtxoError> {
        self.utxos.find_keychain_utxos(keychain_ids).await
    }

    #[instrument(name = "utxos.outpoints_bdk_should_not_select", skip_all, err)]
    pub async fn outpoints_bdk_should_not_select(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        ids: impl Iterator<Item = KeychainId>,
    ) -> Result<HashMap<KeychainId, Vec<OutPoint>>, UtxoError> {
        // Here we list all Utxos that bdk might want to use and lock them (FOR UPDATE)
        // This ensures that we don't have 2 concurrent psbt constructions get in the way
        // of each other
        let reservable_utxos = self.utxos.find_reservable_utxos(tx, ids).await?;

        // We need to tell bdk which utxos not to select.
        // If we have included it in a batch OR
        // it is an income address and not recorded as settled yet
        // we need to flag it to bdk
        let filtered_utxos = reservable_utxos.into_iter().filter_map(|utxo| {
            if utxo.spending_batch_id.is_some()
                || (utxo.income_address && utxo.utxo_settled_ledger_tx_id.is_none())
            {
                Some((utxo.keychain_id, utxo.outpoint))
            } else {
                None
            }
        });

        let mut outpoints_map = HashMap::new();
        for (keychain_id, outpoint) in filtered_utxos {
            outpoints_map
                .entry(keychain_id)
                .or_insert_with(Vec::new)
                .push(outpoint);
        }

        Ok(outpoints_map)
    }

    #[instrument(name = "utxos.reserve_utxos_in_batch", skip_all, err)]
    pub async fn reserve_utxos_in_batch(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        account_id: AccountId,
        batch_id: BatchId,
        payout_queue_id: PayoutQueueId,
        fee_rate: bitcoin::FeeRate,
        utxos: impl IntoIterator<Item = (KeychainId, OutPoint)>,
    ) -> Result<(), UtxoError> {
        self.utxos
            .reserve_utxos_in_batch(tx, account_id, batch_id, payout_queue_id, fee_rate, utxos)
            .await
    }

    pub async fn average_utxo_value(
        &self,
        wallet_id: WalletId,
        queue_id: PayoutQueueId,
    ) -> Result<Option<Satoshis>, UtxoError> {
        self.utxos.average_utxo_value(wallet_id, queue_id).await
    }

    #[instrument(name = "utxos.accounting_info_for_batch", skip_all, err)]
    pub async fn accounting_info_for_batch(
        &self,
        batch_id: BatchId,
        wallet_id: WalletId,
    ) -> Result<
        (
            HashMap<LedgerTransactionId, Vec<bitcoin::OutPoint>>,
            Satoshis,
        ),
        UtxoError,
    > {
        self.utxos
            .accounting_info_for_batch(batch_id, wallet_id)
            .await
    }

    #[instrument(name = "utxos.list_utxos_by_outpoint", skip(self), err)]
    pub async fn list_utxos_by_outpoint(
        &self,
        utxos: &HashMap<KeychainId, Vec<OutPoint>>,
    ) -> Result<Vec<WalletUtxo>, UtxoError> {
        self.utxos.list_utxos_by_outpoint(utxos).await
    }

    #[instrument(name = "utxos.delete_utxo", skip(self), err)]
    pub async fn delete_utxo(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        outpoint: bitcoin::OutPoint,
        keychain_id: KeychainId,
    ) -> Result<LedgerTransactionId, UtxoError> {
        self.utxos.delete_utxo(tx, outpoint, keychain_id).await
    }
}
