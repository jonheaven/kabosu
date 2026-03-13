use bitcoin::Network;

// Canonical Dogecoin network constants mirrored from Dogecoin Core
// (src/chainparams.cpp) to prevent accidental Bitcoin regressions.

pub const AUXPOW_CHAIN_ID: u32 = 0x0062;

pub const MAINNET_MESSAGE_START: [u8; 4] = [0xc0, 0xc0, 0xc0, 0xc0];
pub const TESTNET_MESSAGE_START: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
pub const REGTEST_MESSAGE_START: [u8; 4] = [0xfa, 0xbf, 0xb5, 0xda];

pub const MAINNET_P2P_PORT: u16 = 22556;
pub const TESTNET_P2P_PORT: u16 = 44556;
pub const REGTEST_P2P_PORT: u16 = 18444;

pub const MAINNET_RPC_PORT: u16 = 22555;
pub const TESTNET_RPC_PORT: u16 = 44555;
pub const REGTEST_RPC_PORT: u16 = 18444;

pub const MAINNET_SUBSIDY_HALVING_INTERVAL: u32 = 100_000;
pub const TESTNET_SUBSIDY_HALVING_INTERVAL: u32 = 100_000;
pub const REGTEST_SUBSIDY_HALVING_INTERVAL: u32 = 150;

pub const MAINNET_PUBKEY_PREFIX: u8 = 30;
pub const MAINNET_SCRIPT_PREFIX: u8 = 22;
pub const MAINNET_SECRET_PREFIX: u8 = 158;

pub const TESTNET_PUBKEY_PREFIX: u8 = 113;
pub const TESTNET_SCRIPT_PREFIX: u8 = 196;
pub const TESTNET_SECRET_PREFIX: u8 = 241;

pub const REGTEST_PUBKEY_PREFIX: u8 = 111;
pub const REGTEST_SCRIPT_PREFIX: u8 = 196;
pub const REGTEST_SECRET_PREFIX: u8 = 239;

pub const MAINNET_GENESIS_HASH: &str =
    "1a91e3dace36e2be3bf030a65679fe821aa1d6ef92e7c9902eb318182c355691";
pub const TESTNET_GENESIS_HASH: &str =
    "bb0a78264637406b6360aad926284d544d7049f45189db5664f3c4d07350559e";
pub const REGTEST_GENESIS_HASH: &str =
    "3d2160a3b5dc4a9d62e7e66a295f70313ac808440ef7400d6c0772171ce973a5";

pub fn message_start(network: Network) -> Option<[u8; 4]> {
    match network {
        Network::Bitcoin => Some(MAINNET_MESSAGE_START),
        Network::Testnet | Network::Testnet4 => Some(TESTNET_MESSAGE_START),
        Network::Regtest => Some(REGTEST_MESSAGE_START),
        Network::Signet => None,
    }
}

pub fn rpc_port(network: Network) -> Option<u16> {
    match network {
        Network::Bitcoin => Some(MAINNET_RPC_PORT),
        Network::Testnet | Network::Testnet4 => Some(TESTNET_RPC_PORT),
        Network::Regtest => Some(REGTEST_RPC_PORT),
        Network::Signet => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs, path::PathBuf};

    fn dogecoin_core_chainparams_path() -> PathBuf {
        if let Ok(path) = env::var("DOGECOIN_CORE_CHAINPARAMS") {
            return PathBuf::from(path);
        }

        PathBuf::from("C:/Users/<USER>/Desktop/chains/dogecoin/src/chainparams.cpp")
    }

    #[test]
    fn dogecoin_message_start_values_match_core() {
        assert_eq!(MAINNET_MESSAGE_START, [0xc0, 0xc0, 0xc0, 0xc0]);
        assert_eq!(TESTNET_MESSAGE_START, [0xfc, 0xc1, 0xb7, 0xdc]);
        assert_eq!(REGTEST_MESSAGE_START, [0xfa, 0xbf, 0xb5, 0xda]);
    }

    #[test]
    fn dogecoin_ports_match_core() {
        assert_eq!(MAINNET_P2P_PORT, 22556);
        assert_eq!(TESTNET_P2P_PORT, 44556);
        assert_eq!(REGTEST_P2P_PORT, 18444);

        assert_eq!(MAINNET_RPC_PORT, 22555);
        assert_eq!(TESTNET_RPC_PORT, 44555);
        assert_eq!(REGTEST_RPC_PORT, 18444);
    }

    #[test]
    fn dogecoin_base58_prefixes_match_core() {
        assert_eq!(MAINNET_PUBKEY_PREFIX, 30);
        assert_eq!(MAINNET_SCRIPT_PREFIX, 22);
        assert_eq!(MAINNET_SECRET_PREFIX, 158);

        assert_eq!(TESTNET_PUBKEY_PREFIX, 113);
        assert_eq!(TESTNET_SCRIPT_PREFIX, 196);
        assert_eq!(TESTNET_SECRET_PREFIX, 241);

        assert_eq!(REGTEST_PUBKEY_PREFIX, 111);
        assert_eq!(REGTEST_SCRIPT_PREFIX, 196);
        assert_eq!(REGTEST_SECRET_PREFIX, 239);
    }

    #[test]
    fn dogecoin_subsidy_intervals_match_core() {
        assert_eq!(MAINNET_SUBSIDY_HALVING_INTERVAL, 100_000);
        assert_eq!(TESTNET_SUBSIDY_HALVING_INTERVAL, 100_000);
        assert_eq!(REGTEST_SUBSIDY_HALVING_INTERVAL, 150);
    }

    #[test]
    fn dogecoin_auxpow_chain_id_matches_core() {
        assert_eq!(AUXPOW_CHAIN_ID, 0x0062);
    }

    #[test]
    fn network_mapping_is_explicit() {
        assert_eq!(message_start(Network::Bitcoin), Some(MAINNET_MESSAGE_START));
        assert_eq!(message_start(Network::Testnet), Some(TESTNET_MESSAGE_START));
        assert_eq!(message_start(Network::Regtest), Some(REGTEST_MESSAGE_START));
        assert_eq!(message_start(Network::Signet), None);

        assert_eq!(rpc_port(Network::Bitcoin), Some(MAINNET_RPC_PORT));
        assert_eq!(rpc_port(Network::Testnet), Some(TESTNET_RPC_PORT));
        assert_eq!(rpc_port(Network::Regtest), Some(REGTEST_RPC_PORT));
        assert_eq!(rpc_port(Network::Signet), None);
    }

    #[test]
    fn dogecoin_core_source_matches_constants_when_available() {
        let path = dogecoin_core_chainparams_path();
        if !path.exists() {
            eprintln!(
                "Skipping source-sync check; file not found at {}",
                path.display()
            );
            return;
        }

        let src = fs::read_to_string(&path).expect("unable to read dogecoin core chainparams.cpp");

        // mainnet message start and ports
        assert!(src.contains("pchMessageStart[0] = 0xc0;"));
        assert!(src.contains("pchMessageStart[1] = 0xc0;"));
        assert!(src.contains("pchMessageStart[2] = 0xc0;"));
        assert!(src.contains("pchMessageStart[3] = 0xc0;"));
        assert!(src.contains("nDefaultPort = 22556;"));
        assert!(src.contains("consensus.nSubsidyHalvingInterval = 100000;"));
        assert!(src.contains("base58Prefixes[PUBKEY_ADDRESS] = std::vector<unsigned char>(1,30);"));
        assert!(src.contains("base58Prefixes[SCRIPT_ADDRESS] = std::vector<unsigned char>(1,22);"));

        // testnet message start and ports
        assert!(src.contains("pchMessageStart[0] = 0xfc;"));
        assert!(src.contains("pchMessageStart[1] = 0xc1;"));
        assert!(src.contains("pchMessageStart[2] = 0xb7;"));
        assert!(src.contains("pchMessageStart[3] = 0xdc;"));
        assert!(src.contains("nDefaultPort = 44556;"));
        assert!(src.contains("base58Prefixes[PUBKEY_ADDRESS] = std::vector<unsigned char>(1,113);"));

        // regtest message start and ports
        assert!(src.contains("pchMessageStart[0] = 0xfa;"));
        assert!(src.contains("pchMessageStart[1] = 0xbf;"));
        assert!(src.contains("pchMessageStart[2] = 0xb5;"));
        assert!(src.contains("pchMessageStart[3] = 0xda;"));
        assert!(src.contains("nDefaultPort = 18444;"));
        assert!(src.contains("consensus.nSubsidyHalvingInterval = 150;"));

        // shared AuxPoW chain id
        assert!(src.contains("consensus.nAuxpowChainId = 0x0062;"));
    }
}
