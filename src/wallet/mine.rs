use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bip300301::{
    client::{BoolWitness, GetRawMempoolClient as _},
    MainClient as _,
};
use bitcoin::{
    absolute::{Height, LockTime},
    block::Version as BlockVersion,
    consensus::Encodable as _,
    constants::{genesis_block, SUBSIDY_HALVING_INTERVAL},
    hash_types::TxMerkleNode,
    hashes::Hash as _,
    merkle_tree,
    opcodes::{all::OP_RETURN, OP_0},
    script::PushBytesBuf,
    transaction::Version as TxVersion,
    Amount, Block, BlockHash, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
    Txid, Witness,
};
use futures::{
    stream::{self, FusedStream},
    StreamExt as _,
};
use miette::{miette, IntoDiagnostic as _};

use crate::{
    messages::{CoinbaseBuilder, M4AckBundles},
    types::{Ctip, SidechainAck, WITHDRAWAL_BUNDLE_INCLUSION_THRESHOLD},
    wallet::{error, Wallet},
};

fn get_block_value(height: u32, fees: Amount, network: Network) -> Amount {
    let subsidy_sats = 50 * Amount::ONE_BTC.to_sat();
    let subsidy_halving_interval = match network {
        Network::Regtest => 150,
        _ => SUBSIDY_HALVING_INTERVAL,
    };
    let halvings = height / subsidy_halving_interval;
    if halvings >= 64 {
        fees
    } else {
        fees + Amount::from_sat(subsidy_sats >> halvings)
    }
}

const WITNESS_RESERVED_VALUE: [u8; 32] = [0; 32];

impl Wallet {
    /// Generate coinbase txouts for a new block
    pub(in crate::wallet) fn generate_coinbase_txouts(
        &self,
        ack_all_proposals: bool,
        mainchain_tip: BlockHash,
    ) -> Result<Vec<TxOut>, error::GenerateCoinbaseTxouts> {
        // This is a list of pending sidechain proposals from /our/ wallet, fetched from
        // the DB.
        let sidechain_proposals = self.get_our_sidechain_proposals()?;
        let mut coinbase_builder = CoinbaseBuilder::new();
        for sidechain_proposal in sidechain_proposals {
            coinbase_builder.propose_sidechain(sidechain_proposal)?;
        }

        let mut sidechain_acks = self.get_sidechain_acks()?;

        // This is a map of pending sidechain proposals from the /validator/, i.e.
        // proposals broadcasted by (potentially) someone else, and already active.
        let active_sidechain_proposals = self.get_active_sidechain_proposals()?;

        if ack_all_proposals && !active_sidechain_proposals.is_empty() {
            tracing::info!(
                "Handle sidechain ACK: acking all sidechains regardless of what DB says"
            );

            for (sidechain_number, sidechain_proposal) in &active_sidechain_proposals {
                let sidechain_number = *sidechain_number;

                if !sidechain_acks
                    .iter()
                    .any(|ack| ack.sidechain_number == sidechain_number)
                {
                    tracing::debug!(
                        "Handle sidechain ACK: adding 'fake' ACK for {}",
                        sidechain_number
                    );
                    self.ack_sidechain(
                        sidechain_number,
                        sidechain_proposal.description.sha256d_hash(),
                    )?;
                    sidechain_acks.push(SidechainAck {
                        sidechain_number,
                        description_hash: sidechain_proposal.description.sha256d_hash(),
                    });
                }
            }
        }

        for sidechain_ack in sidechain_acks {
            if !self.validate_sidechain_ack(&sidechain_ack, &active_sidechain_proposals) {
                self.delete_sidechain_ack(&sidechain_ack)?;
                tracing::info!(
                    "Unable to handle sidechain ack, deleted: {}",
                    sidechain_ack.sidechain_number
                );
                continue;
            }

            tracing::debug!(
                "Generate: adding ACK for sidechain {}",
                sidechain_ack.sidechain_number
            );

            coinbase_builder.ack_sidechain(
                sidechain_ack.sidechain_number,
                sidechain_ack.description_hash,
            )?;
        }

        let bmm_hashes = self.get_bmm_requests(&mainchain_tip)?;
        for (sidechain_number, bmm_hash) in &bmm_hashes {
            tracing::info!(
                "Generate: adding BMM accept for SC {} with hash: {}",
                sidechain_number,
                hex::encode(bmm_hash)
            );
            coinbase_builder.bmm_accept(*sidechain_number, bmm_hash)?;
        }
        for (sidechain_id, m6ids) in self.get_bundle_proposals()? {
            for (m6id, _blinded_m6, m6id_info) in m6ids {
                if m6id_info.is_none() {
                    coinbase_builder.propose_bundle(sidechain_id, m6id)?;
                }
            }
        }
        // Ack bundles
        // TODO: Exclusively ack bundles that are known to the wallet
        // TODO: ack bundles when M2 messages are present
        if ack_all_proposals && coinbase_builder.messages().m2_acks().is_empty() {
            let active_sidechains = self.inner.validator.get_active_sidechains()?;
            let upvotes = active_sidechains
                .into_iter()
                .map(|sidechain| {
                    if self
                        .inner
                        .validator
                        .get_pending_withdrawals(&sidechain.proposal.sidechain_number)?
                        .is_empty()
                    {
                        Ok(M4AckBundles::ABSTAIN_ONE_BYTE)
                    } else {
                        Ok(0)
                    }
                })
                .collect::<Result<_, crate::validator::GetPendingWithdrawalsError>>()?;
            coinbase_builder.ack_bundles(M4AckBundles::OneByte { upvotes })?;
        }
        let res = coinbase_builder.build()?;
        Ok(res)
    }

    /// select non-coinbase txs for a new block
    async fn select_block_txs(&self) -> miette::Result<Vec<Transaction>> {
        let mut res = vec![];

        for (sidechain_id, m6ids) in self.get_bundle_proposals()? {
            let mut ctip = None;
            for (_m6id, blinded_m6, m6id_info) in m6ids {
                let Some(m6id_info) = m6id_info else { continue };
                if m6id_info.vote_count > WITHDRAWAL_BUNDLE_INCLUSION_THRESHOLD {
                    let Ctip { outpoint, value } = if let Some(ctip) = ctip {
                        ctip
                    } else {
                        self.inner.validator.get_ctip(sidechain_id)?
                    };
                    let new_value = (value - *blinded_m6.fee()) - *blinded_m6.payout();
                    let m6 = blinded_m6.into_m6(sidechain_id, outpoint, value)?;
                    ctip = Some(Ctip {
                        outpoint: OutPoint {
                            txid: m6.compute_txid(),
                            vout: (m6.output.len() - 1) as u32,
                        },
                        value: new_value,
                    });
                    res.push(m6);
                }
            }
        }

        // We want to include all transactions from the mempool into our newly generated block.
        // This approach is perhaps a bit naive, and could fail if there are conflicting TXs
        // pending. On signet the block is constructed using `getblocktemplate`, so this will not
        // be an issue there.
        //
        // Including all the mempool transactions here ensure that pending sidechain deposit
        // transactions get included into a block.
        let raw_mempool = self
            .inner
            .main_client
            .get_raw_mempool(BoolWitness::<false>, BoolWitness::<false>)
            .await
            .map_err(|err| error::BitcoinCoreRPC {
                method: "getrawmempool".to_string(),
                error: err,
            })?;

        for txid in raw_mempool {
            let transaction = self.fetch_transaction(txid).await?;
            res.push(transaction);
        }

        Ok(res)
    }

    /// Construct a coinbase tx from txouts
    fn finalize_coinbase(
        &self,
        best_block_height: u32,
        coinbase_outputs: &[TxOut],
    ) -> miette::Result<Transaction> {
        let coinbase_addr = self.get_new_address()?;
        tracing::trace!(%coinbase_addr, "Fetched address");
        let coinbase_spk = coinbase_addr.script_pubkey();

        let script_sig = bitcoin::blockdata::script::Builder::new()
            .push_int((best_block_height + 1) as i64)
            .push_opcode(OP_0)
            .into_script();
        let value = get_block_value(best_block_height + 1, Amount::ZERO, Network::Regtest);
        let output = if value > Amount::ZERO {
            vec![TxOut {
                script_pubkey: coinbase_spk,
                value,
            }]
        } else {
            vec![TxOut {
                script_pubkey: ScriptBuf::builder().push_opcode(OP_RETURN).into_script(),
                value: Amount::ZERO,
            }]
        };
        Ok(Transaction {
            version: TxVersion::TWO,
            lock_time: LockTime::Blocks(Height::ZERO),
            input: vec![TxIn {
                previous_output: bitcoin::OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0xFFFF_FFFF,
                },
                sequence: Sequence::MAX,
                witness: Witness::from_slice(&[WITNESS_RESERVED_VALUE]),
                script_sig,
            }],
            output: [&output, coinbase_outputs].concat(),
        })
    }

    /// Finalize a new block by constructing the coinbase tx
    fn finalize_block(
        &self,
        coinbase_outputs: &[TxOut],
        transactions: Vec<Transaction>,
    ) -> miette::Result<Block> {
        let best_block_hash = self.validator().get_mainchain_tip()?;
        let best_block_height = self.validator().get_header_info(&best_block_hash)?.height;
        tracing::trace!(%best_block_hash, %best_block_height, "Found mainchain tip");

        let coinbase_tx = self.finalize_coinbase(best_block_height, coinbase_outputs)?;
        let txdata = std::iter::once(coinbase_tx).chain(transactions).collect();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .into_diagnostic()?
            .as_secs() as u32;
        let genesis_block = genesis_block(bitcoin::Network::Regtest);
        let bits = genesis_block.header.bits;
        let header = bitcoin::block::Header {
            version: BlockVersion::NO_SOFT_FORK_SIGNALLING,
            prev_blockhash: best_block_hash,
            // merkle root is computed after the witness commitment is added to coinbase
            merkle_root: TxMerkleNode::all_zeros(),
            time: timestamp,
            bits,
            nonce: 0,
        };
        let mut block = Block { header, txdata };
        let witness_root = block.witness_root().unwrap();
        let witness_commitment =
            Block::compute_witness_commitment(&witness_root, &WITNESS_RESERVED_VALUE);

        // https://github.com/bitcoin/bips/blob/master/bip-0141.mediawiki#commitment-structure
        const WITNESS_COMMITMENT_HEADER: [u8; 4] = [0xaa, 0x21, 0xa9, 0xed];
        let witness_commitment_spk = {
            let mut push_bytes = PushBytesBuf::from(WITNESS_COMMITMENT_HEADER);
            let () = push_bytes
                .extend_from_slice(witness_commitment.as_byte_array())
                .into_diagnostic()?;
            ScriptBuf::new_op_return(push_bytes)
        };
        block.txdata[0].output.push(TxOut {
            script_pubkey: witness_commitment_spk,
            value: bitcoin::Amount::ZERO,
        });
        let mut tx_hashes: Vec<_> = block.txdata.iter().map(Transaction::compute_txid).collect();
        block.header.merkle_root = merkle_tree::calculate_root_inline(&mut tx_hashes)
            .unwrap()
            .to_raw_hash()
            .into();
        Ok(block)
    }

    /// Mine a block
    async fn mine(
        &self,
        coinbase_outputs: &[TxOut],
        transactions: Vec<Transaction>,
    ) -> miette::Result<BlockHash> {
        let transaction_count = transactions.len();

        let mut block = self.finalize_block(coinbase_outputs, transactions)?;
        loop {
            block.header.nonce += 1;
            if block.header.validate_pow(block.header.target()).is_ok() {
                break;
            }
        }
        let mut block_bytes = vec![];
        block
            .consensus_encode(&mut block_bytes)
            .map_err(error::EncodeBlock)?;
        let () = self
            .inner
            .main_client
            .submit_block(hex::encode(block_bytes))
            .await
            .map_err(|err| error::BitcoinCoreRPC {
                method: "submitblock".to_string(),
                error: err,
            })?;
        let block_hash = block.header.block_hash();
        tracing::info!(%block_hash, %transaction_count, "Submitted block");
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(block_hash)
    }

    /// Build and mine a single block
    async fn generate_block(&self, ack_all_proposals: bool) -> miette::Result<BlockHash> {
        let Some(mainchain_tip) = self.inner.validator.try_get_mainchain_tip()? else {
            return Err(miette!("Validator is not synced"));
        };
        let coinbase_outputs = self.generate_coinbase_txouts(ack_all_proposals, mainchain_tip)?;
        let transactions = self.select_block_txs().await?;

        tracing::info!(
            coinbase_outputs = %coinbase_outputs.len(),
            transactions = %transactions.len(),
            "Mining block",
        );

        let block_hash = self.mine(&coinbase_outputs, transactions).await?;
        self.delete_pending_sidechain_proposals()?;
        self.delete_bmm_requests(&mainchain_tip)?;
        Ok(block_hash)
    }

    pub fn generate_blocks<Ref>(
        this: Ref,
        count: u32,
        ack_all_proposals: bool,
    ) -> impl FusedStream<Item = miette::Result<BlockHash>>
    where
        Ref: std::borrow::Borrow<Self>,
    {
        tracing::info!("Generate: creating {} blocks", count);
        stream::try_unfold((this, count), move |(this, remaining)| async move {
            if remaining == 0 {
                Ok(None)
            } else {
                let block_hash = this.borrow().generate_block(ack_all_proposals).await?;
                Ok(Some((block_hash, (this, remaining - 1))))
            }
        })
        .fuse()
    }
}
