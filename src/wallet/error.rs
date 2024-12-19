use bip300301::jsonrpsee::core::client::Error as JsonRpcError;
use miette::{diagnostic, Diagnostic};
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Diagnostic, Error)]
#[diagnostic(
    code(electrum_error),
    help("The error is from the Electrum server. Check the message for more details.")
)]
#[error("electrum error `{code}`: `{message}`")]
pub struct ElectrumError {
    code: i32,
    message: String,
}

impl From<ElectrumError> for tonic::Status {
    fn from(error: ElectrumError) -> Self {
        let code = match error.code {
            // https://github.com/bitcoin/bitcoin/blob/e8f72aefd20049eac81b150e7f0d33709acd18ed/src/common/messages.cpp
            -25 => tonic::Code::InvalidArgument,
            _ => tonic::Code::Unknown,
        };
        Self::new(code, error.to_string())
    }
}

#[derive(Debug, Diagnostic, Error)]
#[error("Bitcoin Core RPC error `{method}")]
#[diagnostic(code(bitcoin_core_rpc_error))]
pub struct BitcoinCoreRPC {
    pub method: String,
    #[source]
    pub error: JsonRpcError,
}

#[derive(Debug, Diagnostic, Error)]
#[error("failed to consensus encode block")]
#[diagnostic(code(encode_block_error))]
pub struct EncodeBlock(#[from] pub bitcoin::io::Error);

#[derive(Debug, Diagnostic, Error)]
pub(in crate::wallet) enum GetBundleProposals {
    #[error(transparent)]
    BlindedM6(#[from] crate::types::BlindedM6Error),
    #[error(transparent)]
    ConsensusEncoding(#[from] bitcoin::consensus::encode::Error),
    #[error(transparent)]
    GetPendingWithdrawals(#[from] crate::validator::GetPendingWithdrawalsError),
    #[error(transparent)]
    Rustqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Diagnostic, Error)]
pub(in crate::wallet) enum GenerateCoinbaseTxouts {
    #[error(transparent)]
    CoinbaseMessages(#[from] crate::messages::CoinbaseMessagesError),
    #[error(transparent)]
    GetBundleProposals(#[from] crate::wallet::error::GetBundleProposals),
    #[error(transparent)]
    GetPendingWithdrawals(#[from] crate::validator::GetPendingWithdrawalsError),
    #[error(transparent)]
    GetSidechains(#[from] crate::validator::GetSidechainsError),
    #[error(transparent)]
    PushBytes(#[from] bitcoin::script::PushBytesError),
    #[error(transparent)]
    Rustqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Diagnostic, Error)]
pub(in crate::wallet) enum InitialBlockTemplateInner {
    #[error(transparent)]
    GetMainchainTip(#[from] crate::validator::GetMainchainTipError),
    #[error(transparent)]
    GenerateCoinbaseTxouts(#[from] GenerateCoinbaseTxouts),
}

#[derive(Debug, Diagnostic, Error)]
#[error(transparent)]
#[repr(transparent)]
pub struct InitialBlockTemplate(InitialBlockTemplateInner);

impl<Err> From<Err> for InitialBlockTemplate
where
    InitialBlockTemplateInner: From<Err>,
{
    fn from(err: Err) -> Self {
        Self(err.into())
    }
}
