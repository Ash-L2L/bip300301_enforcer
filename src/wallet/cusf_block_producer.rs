use std::collections::HashMap;

use bitcoin::{BlockHash, Transaction, Txid};
use cusf_enforcer_mempool::{
    cusf_block_producer::{
        typewit::const_marker::{Bool, BoolWit},
        CoinbaseTxn, CoinbaseTxouts, CusfBlockProducer, InitialBlockTemplate,
    },
    cusf_enforcer::{ConnectBlockAction, CusfEnforcer},
};

use crate::{
    validator::Validator,
    wallet::{error, Wallet},
};

impl CusfEnforcer for Wallet {
    type SyncError = <Validator as CusfEnforcer>::SyncError;

    async fn sync_to_tip(&mut self, tip: BlockHash) -> std::result::Result<(), Self::SyncError> {
        self.inner.validator.clone().sync_to_tip(tip).await
    }

    type ConnectBlockError = <Validator as CusfEnforcer>::ConnectBlockError;

    fn connect_block(
        &mut self,
        block: &bitcoin::Block,
    ) -> Result<ConnectBlockAction, Self::ConnectBlockError> {
        self.inner.validator.clone().connect_block(block)
    }

    type DisconnectBlockError = <Validator as CusfEnforcer>::DisconnectBlockError;

    fn disconnect_block(
        &mut self,
        block_hash: BlockHash,
    ) -> std::result::Result<(), Self::DisconnectBlockError> {
        self.inner.validator.clone().disconnect_block(block_hash)
    }

    type AcceptTxError = <Validator as CusfEnforcer>::AcceptTxError;

    fn accept_tx<TxRef>(
        &mut self,
        tx: &Transaction,
        tx_inputs: &HashMap<Txid, TxRef>,
    ) -> std::result::Result<bool, Self::AcceptTxError>
    where
        TxRef: std::borrow::Borrow<Transaction>,
    {
        self.inner.validator.clone().accept_tx(tx, tx_inputs)
    }
}

impl CusfBlockProducer for Wallet {
    type InitialBlockTemplateError = error::InitialBlockTemplate;

    fn initial_block_template<const COINBASE_TXN: bool>(
        &self,
        coinbase_txn_wit: BoolWit<COINBASE_TXN>,
        mut template: InitialBlockTemplate<COINBASE_TXN>,
    ) -> Result<InitialBlockTemplate<COINBASE_TXN>, Self::InitialBlockTemplateError>
    where
        Bool<COINBASE_TXN>: CoinbaseTxn,
    {
        if let BoolWit::True(wit) = coinbase_txn_wit {
            let mainchain_tip = self.validator().get_mainchain_tip()?;
            let wit = wit.map(CoinbaseTxouts);
            let coinbase_txouts: &mut Vec<_> = wit.in_mut().to_right(&mut template.coinbase_txouts);
            coinbase_txouts.extend(self.generate_coinbase_txouts(true, mainchain_tip)?);
        }
        // FIXME: set prefix txns and exclude mempool txs
        Ok(template)
    }

    // FIXME: implement
    type SuffixTxsError = std::convert::Infallible;

    fn suffix_txs<const COINBASE_TXN: bool>(
        &self,
        _coinbase_txn_wit: BoolWit<COINBASE_TXN>,
        _template: &InitialBlockTemplate<COINBASE_TXN>,
    ) -> Result<Vec<(Transaction, bitcoin::Amount)>, Self::SuffixTxsError>
    where
        Bool<COINBASE_TXN>: CoinbaseTxn,
    {
        // FIXME: implement
        Ok(Vec::new())
    }
}
