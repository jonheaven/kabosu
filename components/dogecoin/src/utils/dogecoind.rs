use std::{thread::sleep, time::Duration};

use bitcoincore_rpc::{
    bitcoin::{BlockHash, Txid},
    Auth, Client, RpcApi,
};
use bitcoincore_rpc_json::GetRawTransactionResult;
use config::DogecoinConfig;

use crate::{try_error, try_info, types::BlockIdentifier, utils::Context};

pub fn dogecoin_get_client(config: &DogecoinConfig, ctx: &Context) -> Client {
    loop {
        let auth = Auth::UserPass(config.rpc_username.clone(), config.rpc_password.clone());
        match Client::new(&config.rpc_url, auth) {
            Ok(con) => {
                return con;
            }
            Err(e) => {
                try_error!(ctx, "dogecoind: Unable to get client: {}", e.to_string());
                sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Retrieves the chain tip from dogecoind.
pub fn dogecoin_get_chain_tip(config: &DogecoinConfig, ctx: &Context) -> BlockIdentifier {
    let bitcoin_rpc = dogecoin_get_client(config, ctx);
    loop {
        match (
            bitcoin_rpc.get_block_count(),
            bitcoin_rpc.get_best_block_hash(),
        ) {
            (Ok(blocks), Ok(best_block_hash)) => {
                return BlockIdentifier {
                    index: blocks,
                    hash: format!("0x{}", best_block_hash),
                };
            }
            (Err(e), _) | (_, Err(e)) => {
                try_error!(
                    ctx,
                    "dogecoind: Unable to get block height: {}",
                    e.to_string()
                );
                sleep(Duration::from_secs(1));
            }
        };
    }
}

/// Retrieves the block_height for a given blockhash.
pub fn dogecoin_get_block_height(
    bitcoin_rpc: &Client,
    ctx: &Context,
    blockhash: &BlockHash,
) -> Result<u32, String> {
    bitcoin_rpc
        .get_block_header_info(blockhash)
        .map(|result| result.height.try_into().unwrap())
        .map_err(|e| {
            try_error!(
                ctx,
                "dogecoind: Unable to get block header info: {}",
                e.to_string()
            );
            e.to_string()
        })
}

/// Retrieves the raw transaction for a given txid.
pub fn bitcoin_get_raw_transaction(
    bitcoin_rpc: &Client,
    ctx: &Context,
    txid: &Txid,
) -> Result<GetRawTransactionResult, String> {
    bitcoin_rpc
        .get_raw_transaction_info(txid, None)
        .map_err(|e| {
            try_error!(ctx, "dogecoind: Unable to get raw transaction: {e}",);
            e.to_string()
        })
}

/// Checks if dogecoind is still synchronizing blocks and waits until it's finished if that is the case.
pub fn dogecoin_wait_for_chain_tip(config: &DogecoinConfig, ctx: &Context) -> BlockIdentifier {
    let bitcoin_rpc = dogecoin_get_client(config, ctx);
    let mut confirmations = 0u8;
    let mut last_tip_hash: Option<BlockHash> = None;
    let mut last_tip_height: Option<u64> = None;
    let mut logged_info = false;
    loop {
        match (
            bitcoin_rpc.get_block_count(),
            bitcoin_rpc.get_best_block_hash(),
        ) {
            (Ok(blocks), Ok(best_block_hash)) => {
                let same_tip = last_tip_hash.as_ref() == Some(&best_block_hash)
                    && last_tip_height == Some(blocks);

                if same_tip {
                    confirmations = confirmations.saturating_add(1);
                } else {
                    confirmations = 0;
                }

                last_tip_hash = Some(best_block_hash);
                last_tip_height = Some(blocks);

                // Wait for 10 stable reads before declaring node ready.
                if confirmations >= 10 {
                    try_info!(ctx, "dogecoind chain tip is at block #{}", blocks);
                    return BlockIdentifier {
                        index: blocks,
                        hash: format!("0x{}", last_tip_hash.expect("tip hash set")),
                    };
                }

                if !logged_info {
                    try_info!(ctx, "dogecoind verifying chain tip...");
                    logged_info = true;
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                try_error!(ctx, "dogecoind error checking for chain tip: {e}");
            }
        };
        sleep(Duration::from_secs(1));
    }
}
