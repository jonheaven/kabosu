use std::{
    fs::File,
    io::{BufReader, Read},
};

use bitcoin::Network;

use crate::{
    DogecoinConfig, Config, DoginalsPredicatesConfig, MetricsConfig, DoginalDrc20Config, DoginalConfig,
    DoginalMetaProtocolsConfig, PgDatabaseConfig, ResourcesConfig, DunesConfig, StorageConfig,
    ProtocolsConfig, DnsProtocolConfig, DogemapProtocolConfig, LottoProtocolConfig, WebhooksConfig,
    DEFAULT_DOGECOIN_RPC_THREADS, DEFAULT_DOGECOIN_RPC_TIMEOUT, DEFAULT_INDEXER_CHANNEL_CAPACITY,
    DEFAULT_LRU_CACHE_SIZE, DEFAULT_MEMORY_AVAILABLE, DEFAULT_ULIMIT, DEFAULT_WORKING_DIR,
};

#[derive(Deserialize, Clone, Debug)]
pub struct PgDatabaseConfigToml {
    pub database: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub search_path: Option<String>,
    pub pool_max_size: Option<usize>,
}

impl PgDatabaseConfigToml {
    fn to_config(&self) -> PgDatabaseConfig {
        PgDatabaseConfig {
            dbname: self.database.clone(),
            host: self.host.clone(),
            port: self.port,
            user: self.username.clone(),
            password: self.password.clone(),
            search_path: self.search_path.clone(),
            pool_max_size: self.pool_max_size,
        }
    }
}

/// Hiro-style predicate-driven selective indexing for Doginals (matches Chainhook/Ordhook design).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct DoginalsPredicatesToml {
    pub enabled: Option<bool>,
    pub mime_types: Option<Vec<String>>,
    pub content_prefixes: Option<Vec<String>>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DoginalConfigToml {
    pub db: PgDatabaseConfigToml,
    pub meta_protocols: Option<DoginalMetaProtocolsConfigToml>,
    pub predicates: Option<DoginalsPredicatesToml>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DoginalMetaProtocolsConfigToml {
    pub drc20: Option<DoginalDrc20ConfigToml>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DoginalDrc20ConfigToml {
    pub enabled: bool,
    pub lru_cache_size: Option<usize>,
    pub db: PgDatabaseConfigToml,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DunesConfigToml {
    pub lru_cache_size: Option<usize>,
    pub db: PgDatabaseConfigToml,
}

#[derive(Deserialize, Debug, Clone)]
pub struct StorageConfigToml {
    pub working_dir: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourcesConfigToml {
    pub ulimit: Option<usize>,
    pub cpu_core_available: Option<usize>,
    pub memory_available: Option<usize>,
    pub dogecoin_rpc_threads: Option<usize>,
    pub dogecoin_rpc_timeout: Option<u32>,
    pub indexer_channel_capacity: Option<usize>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct DogecoinConfigToml {
    pub network: String,
    pub rpc_url: String,
    pub rpc_username: Option<String>,
    pub rpc_password: Option<String>,
    pub zmq_url: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MetricsConfigToml {
    pub enabled: bool,
    pub prometheus_port: u16,
}

/// Per-protocol enable/disable (all default to `true` when absent).
#[derive(Deserialize, Debug, Clone, Default)]
pub struct ProtocolsConfigToml {
    pub dns: Option<DnsProtocolConfigToml>,
    pub dogemap: Option<DogemapProtocolConfigToml>,
    pub lotto: Option<LottoProtocolConfigToml>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LottoProtocolConfigToml {
    pub enabled: Option<bool>,
    pub content_prefixes: Option<Vec<String>>,
    pub burn_address: Option<String>,
    pub protocol_dev_address: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct DnsProtocolConfigToml {
    pub enabled: Option<bool>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct DogemapProtocolConfigToml {
    pub enabled: Option<bool>,
}

/// Webhook delivery — optional section; when absent, webhooks are disabled.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct WebhooksConfigToml {
    pub enabled: Option<bool>,
    pub urls: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ConfigToml {
    pub storage: StorageConfigToml,
    pub doginals: Option<DoginalConfigToml>,
    pub dunes: Option<DunesConfigToml>,
    pub dogecoin: DogecoinConfigToml,
    pub resources: ResourcesConfigToml,
    pub metrics: Option<MetricsConfigToml>,
    pub protocols: Option<ProtocolsConfigToml>,
    pub webhooks: Option<WebhooksConfigToml>,
}

impl ConfigToml {
    pub fn config_from_file_path(file_path: &str) -> Result<Config, String> {
        let file = File::open(file_path)
            .map_err(|e| format!("unable to read file {}\n{:?}", file_path, e))?;
        let mut file_reader = BufReader::new(file);
        let mut file_buffer = vec![];
        file_reader
            .read_to_end(&mut file_buffer)
            .map_err(|e| format!("unable to read file {}\n{:?}", file_path, e))?;

        let config_file: ConfigToml = match toml::from_slice(&file_buffer) {
            Ok(s) => s,
            Err(e) => {
                return Err(format!("Config file malformatted {}", e));
            }
        };
        ConfigToml::config_from_toml(config_file)
    }

    fn config_from_toml(toml: ConfigToml) -> Result<Config, String> {
        let bitcoin_network = match toml.dogecoin.network.as_str() {
            "devnet" => Network::Regtest,
            "testnet" => Network::Testnet,
            "mainnet" => Network::Bitcoin,
            _ => {
                return Err(
                    "dogecoin.network not supported (expected one of: mainnet, testnet, devnet)"
                        .to_string(),
                )
            }
        };
        let ordinals = match toml.doginals {
            Some(ordinals) => Some(DoginalConfig {
                db: ordinals.db.to_config(),
                meta_protocols: match ordinals.meta_protocols {
                    Some(meta_protocols) => Some(DoginalMetaProtocolsConfig {
                        drc20: match meta_protocols.drc20 {
                            Some(drc20) => Some(DoginalDrc20Config {
                                enabled: drc20.enabled,
                                lru_cache_size: drc20
                                    .lru_cache_size
                                    .unwrap_or(DEFAULT_LRU_CACHE_SIZE),
                                db: drc20.db.to_config(),
                            }),
                            None => None,
                        },
                    }),
                    None => None,
                },
                predicates: ordinals.predicates.map(|p| DoginalsPredicatesConfig {
                    enabled: p.enabled.unwrap_or(false),
                    mime_types: p.mime_types.unwrap_or_default(),
                    content_prefixes: p.content_prefixes.unwrap_or_default(),
                }),
            }),
            None => None,
        };
        let runes = match toml.dunes {
            Some(runes) => Some(DunesConfig {
                lru_cache_size: runes.lru_cache_size.unwrap_or(DEFAULT_LRU_CACHE_SIZE),
                db: runes.db.to_config(),
            }),
            None => None,
        };
        let metrics = toml.metrics.map(|metrics| MetricsConfig {
            enabled: metrics.enabled,
            prometheus_port: metrics.prometheus_port,
        });

        let protocols = {
            let p = toml.protocols.unwrap_or_default();
            ProtocolsConfig {
                dns: DnsProtocolConfig {
                    enabled: p.dns.as_ref().and_then(|d| d.enabled).unwrap_or(true),
                },
                dogemap: DogemapProtocolConfig {
                    enabled: p.dogemap.as_ref().and_then(|d| d.enabled).unwrap_or(true),
                },
                lotto: LottoProtocolConfig {
                    enabled: p.lotto.as_ref().and_then(|l| l.enabled).unwrap_or(true),
                    content_prefixes: p
                        .lotto
                        .as_ref()
                        .and_then(|l| l.content_prefixes.clone())
                        .unwrap_or_else(|| vec![r#"{"p":"doge-lotto""#.to_string()]),
                    burn_address: p
                        .lotto
                        .as_ref()
                        .and_then(|l| l.burn_address.clone())
                        .unwrap_or_else(|| "DBurnXXXXXXXXXXXXXXXXXXXXXXX9eVvaA".to_string()),
                    protocol_dev_address: p
                        .lotto
                        .as_ref()
                        .and_then(|l| l.protocol_dev_address.clone())
                        .unwrap_or_default(),
                },
            }
        };

        let webhooks = {
            let w = toml.webhooks.unwrap_or_default();
            WebhooksConfig {
                enabled: w.enabled.unwrap_or(false),
                urls: w.urls.unwrap_or_default(),
            }
        };

        let config = Config {
            storage: StorageConfig {
                working_dir: toml
                    .storage
                    .working_dir
                    .unwrap_or(DEFAULT_WORKING_DIR.into()),
            },
            doginals: ordinals,
            dunes: runes,
            resources: ResourcesConfig {
                ulimit: toml.resources.ulimit.unwrap_or(DEFAULT_ULIMIT),
                cpu_core_available: toml.resources.cpu_core_available.unwrap_or(num_cpus::get()),
                memory_available: toml
                    .resources
                    .memory_available
                    .unwrap_or(DEFAULT_MEMORY_AVAILABLE),
                dogecoin_rpc_threads: toml
                    .resources
                    .dogecoin_rpc_threads
                    .unwrap_or(DEFAULT_DOGECOIN_RPC_THREADS),
                dogecoin_rpc_timeout: toml
                    .resources
                    .dogecoin_rpc_timeout
                    .unwrap_or(DEFAULT_DOGECOIN_RPC_TIMEOUT),
                indexer_channel_capacity: toml
                    .resources
                    .indexer_channel_capacity
                    .unwrap_or(DEFAULT_INDEXER_CHANNEL_CAPACITY),
            },
            dogecoin: DogecoinConfig {
                rpc_url: toml.dogecoin.rpc_url.to_string(),
                rpc_username: toml.dogecoin.rpc_username
                    .or_else(|| std::env::var("DOGE_RPC_USERNAME").ok())
                    .ok_or("dogecoin.rpc_username missing (set in doghook.toml or DOGE_RPC_USERNAME env var)")?,
                rpc_password: toml.dogecoin.rpc_password
                    .or_else(|| std::env::var("DOGE_RPC_PASSWORD").ok())
                    .ok_or("dogecoin.rpc_password missing (set in doghook.toml or DOGE_RPC_PASSWORD env var)")?,
                network: bitcoin_network,
                zmq_url: toml.dogecoin.zmq_url,
            },
            metrics,
            protocols,
            webhooks,
        };
        Ok(config)
    }
}
