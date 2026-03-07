use std::path::PathBuf;

use bitcoin::Network;

use crate::toml::ConfigToml;

pub const DEFAULT_WORKING_DIR: &str = "data";
pub const DEFAULT_ULIMIT: usize = 2048;
pub const DEFAULT_MEMORY_AVAILABLE: usize = 8;
pub const DEFAULT_DOGECOIN_RPC_THREADS: usize = 4;
pub const DEFAULT_DOGECOIN_RPC_TIMEOUT: u32 = 15;
pub const DEFAULT_LRU_CACHE_SIZE: usize = 50_000;
pub const DEFAULT_INDEXER_CHANNEL_CAPACITY: usize = 10;

#[derive(Clone, Debug)]
pub struct Config {
    pub dogecoin: DogecoinConfig,
    pub doginals: Option<DoginalConfig>,
    pub dunes: Option<DunesConfig>,
    pub resources: ResourcesConfig,
    pub storage: StorageConfig,
    pub metrics: Option<MetricsConfig>,
    pub protocols: ProtocolsConfig,
    pub webhooks: WebhooksConfig,
}

/// Webhook delivery configuration.
/// POST requests are fired for each DNS registration and Dogemap claim.
#[derive(Clone, Debug, Default)]
pub struct WebhooksConfig {
    pub enabled: bool,
    /// List of URLs that will receive POST requests for every event.
    pub urls: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DoginalConfig {
    pub db: PgDatabaseConfig,
    pub meta_protocols: Option<DoginalMetaProtocolsConfig>,
    /// Hiro-style predicate filtering: only index inscriptions matching these rules.
    /// When predicates.enabled = false (default), all inscriptions are indexed.
    pub predicates: Option<DoginalsPredicatesConfig>,
}

/// Per-protocol enable/disable switches.
/// Absent from toml = all enabled by default (backward compatible).
#[derive(Clone, Debug)]
pub struct ProtocolsConfig {
    pub dns: DnsProtocolConfig,
    pub dogemap: DogemapProtocolConfig,
    pub lotto: LottoProtocolConfig,
}

impl Default for ProtocolsConfig {
    fn default() -> Self {
        Self {
            dns: DnsProtocolConfig { enabled: true },
            dogemap: DogemapProtocolConfig { enabled: true },
            lotto: LottoProtocolConfig {
                enabled: true,
                content_prefixes: vec![r#"{"p":"doge-lotto""#.to_string()],
                burn_address: "DBurnXXXXXXXXXXXXXXXXXXXXXXX9eVvaA".to_string(),
                protocol_dev_address: String::new(),
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct LottoProtocolConfig {
    pub enabled: bool,
    pub content_prefixes: Vec<String>,
    pub burn_address: String,
    pub protocol_dev_address: String,
}

#[derive(Clone, Debug)]
pub struct DnsProtocolConfig {
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct DogemapProtocolConfig {
    pub enabled: bool,
}

/// Hiro-style predicate-driven selective indexing for Doginals (matches Chainhook/Ordhook design).
#[derive(Clone, Debug, Default)]
pub struct DoginalsPredicatesConfig {
    pub enabled: bool,
    /// Only index inscriptions whose content-type starts with one of these. Empty = any.
    pub mime_types: Vec<String>,
    /// Only index inscriptions whose body starts with one of these UTF-8 prefixes. Empty = any.
    pub content_prefixes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct DoginalMetaProtocolsConfig {
    pub drc20: Option<DoginalDrc20Config>,
}

#[derive(Clone, Debug)]
pub struct DoginalDrc20Config {
    pub enabled: bool,
    pub lru_cache_size: usize,
    pub db: PgDatabaseConfig,
}

#[derive(Clone, Debug)]
pub struct DunesConfig {
    pub lru_cache_size: usize,
    pub db: PgDatabaseConfig,
}

#[derive(Clone, Debug)]
pub struct DogecoinConfig {
    pub network: Network,
    pub rpc_url: String,
    pub rpc_username: String,
    pub rpc_password: String,
    pub zmq_url: String,
}

/// A Postgres configuration for a single database.
#[derive(Clone, Debug)]
pub struct PgDatabaseConfig {
    pub dbname: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub search_path: Option<String>,
    pub pool_max_size: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub working_dir: String,
}

#[derive(Clone, Debug)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub prometheus_port: u16,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ResourcesConfig {
    pub ulimit: usize,
    pub cpu_core_available: usize,
    pub memory_available: usize,
    pub dogecoin_rpc_threads: usize,
    pub dogecoin_rpc_timeout: u32,
    pub indexer_channel_capacity: usize,
}

impl ResourcesConfig {
    pub fn get_optimal_thread_pool_capacity(&self) -> usize {
        // Generally speaking when dealing a pool, we need one thread for
        // feeding the thread pool and eventually another thread for
        // handling the "reduce" step.
        self.cpu_core_available.saturating_sub(2).max(1)
    }
}

impl Config {
    pub fn from_file_path(file_path: &str) -> Result<Config, String> {
        ConfigToml::config_from_file_path(file_path)
    }

    pub fn expected_cache_path(&self) -> PathBuf {
        let mut destination_path = PathBuf::new();
        destination_path.push(&self.storage.working_dir);
        destination_path
    }

    pub fn devnet_default() -> Config {
        Config {
            storage: StorageConfig {
                working_dir: default_cache_path(),
            },
            resources: ResourcesConfig {
                cpu_core_available: num_cpus::get(),
                memory_available: DEFAULT_MEMORY_AVAILABLE,
                ulimit: DEFAULT_ULIMIT,
                dogecoin_rpc_threads: DEFAULT_DOGECOIN_RPC_THREADS,
                dogecoin_rpc_timeout: DEFAULT_DOGECOIN_RPC_TIMEOUT,
                indexer_channel_capacity: DEFAULT_INDEXER_CHANNEL_CAPACITY,
            },
            dogecoin: DogecoinConfig {
                rpc_url: "http://0.0.0.0:18443".into(),
                rpc_username: "devnet".into(),
                rpc_password: "devnet".into(),
                network: Network::Regtest,
                zmq_url: "http://0.0.0.0:18543".into(),
            },
            doginals: Some(DoginalConfig {
                db: PgDatabaseConfig {
                    dbname: "ordinals".to_string(),
                    host: "localhost".to_string(),
                    port: 5432,
                    user: "postgres".to_string(),
                    password: Some("postgres".to_string()),
                    search_path: None,
                    pool_max_size: None,
                },
                meta_protocols: None,
                predicates: None,
            }),
            dunes: Some(DunesConfig {
                lru_cache_size: DEFAULT_LRU_CACHE_SIZE,
                db: PgDatabaseConfig {
                    dbname: "runes".to_string(),
                    host: "localhost".to_string(),
                    port: 5432,
                    user: "postgres".to_string(),
                    password: Some("postgres".to_string()),
                    search_path: None,
                    pool_max_size: None,
                },
            }),
            metrics: Some(MetricsConfig {
                enabled: true,
                prometheus_port: 9153,
            }),
            protocols: ProtocolsConfig::default(),
            webhooks: WebhooksConfig::default(),
        }
    }

    pub fn testnet_default() -> Config {
        let mut default = Config::devnet_default();
        default.dogecoin.network = Network::Testnet;
        default
    }

    pub fn mainnet_default() -> Config {
        let mut default = Config::devnet_default();
        default.dogecoin.rpc_url = "http://localhost:8332".into();
        default.dogecoin.network = Network::Bitcoin;
        default
    }

    // TODO: Move this to a shared test utils component
    pub fn test_default() -> Config {
        let mut config = Self::mainnet_default();
        config.storage.working_dir = "tmp".to_string();
        config.resources.dogecoin_rpc_threads = 1;
        config.resources.cpu_core_available = 1;
        config
    }

    pub fn lotto_enabled(&self) -> bool {
        self.protocols.lotto.enabled
    }

    pub fn webhook_urls(&self) -> &[String] {
        if self.webhooks.enabled {
            &self.webhooks.urls
        } else {
            &[]
        }
    }

    pub fn dns_enabled(&self) -> bool {
        self.protocols.dns.enabled
    }

    pub fn dogemap_enabled(&self) -> bool {
        self.protocols.dogemap.enabled
    }

    pub fn doginals_predicates(&self) -> Option<&DoginalsPredicatesConfig> {
        self.doginals.as_ref()?.predicates.as_ref()
    }

    pub fn ordinals_drc20_config(&self) -> Option<&DoginalDrc20Config> {
        if let Some(DoginalConfig {
            meta_protocols:
                Some(DoginalMetaProtocolsConfig {
                    drc20: Some(drc20), ..
                }),
            ..
        }) = &self.doginals
        {
            if drc20.enabled {
                return Some(drc20);
            }
        }
        None
    }

    pub fn assert_doginals_config(&self) -> Result<(), String> {
        if self.doginals.is_none() {
            return Err("Config entry for `ordinals` not found in config file.".to_string());
        }
        Ok(())
    }

    pub fn assert_dunes_config(&self) -> Result<(), String> {
        if self.dunes.is_none() {
            return Err("Config entry for `runes` not found in config file.".to_string());
        }
        Ok(())
    }
}

pub fn default_cache_path() -> String {
    let mut cache_path = std::env::current_dir().expect("unable to get current dir");
    cache_path.push("data");
    format!("{}", cache_path.display())
}
