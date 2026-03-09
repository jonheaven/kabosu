use clap::{Parser, Subcommand};

/// doghook — Dogecoin Doginals / DNS / Dogemap / Dunes indexer
#[derive(Parser, Debug)]
#[clap(name = "doghook", author, version, about, long_about = None)]
pub enum Protocol {
    /// Doginals index commands
    #[clap(subcommand)]
    Doginals(Command),
    /// Dunes index commands
    #[clap(subcommand)]
    Dunes(Command),
    /// Dogecoin Name System (DNS) query commands
    #[clap(subcommand)]
    Dns(DnsCommand),
    /// Dogemap (block claim) query commands
    #[clap(subcommand)]
    Dogemap(DogemapCommand),
    /// doge-lotto deploy, mint, and query commands
    #[clap(subcommand)]
    Lotto(LottoCommand),
    /// Dogetag on-chain graffiti query commands
    #[clap(subcommand)]
    Dogetag(DogetagCommand),
    /// Configuration file commands
    #[clap(subcommand)]
    Config(ConfigCommand),
}

// ---------------------------------------------------------------------------
// Dogetag subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum DogetagCommand {
    /// List recent on-chain graffiti tags
    #[clap(name = "list")]
    List(DogetagListCommand),
    /// Search tags by message content
    #[clap(name = "search")]
    Search(DogetagSearchCommand),
    /// List tags left by a specific address
    #[clap(name = "address")]
    Address(DogetagAddressCommand),
    /// Send DOGE to an address and burn a message into the same transaction
    #[clap(name = "send")]
    Send(DogetagSendCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DogetagListCommand {
    /// Maximum number of results
    #[clap(long, default_value = "50")]
    pub limit: usize,
    /// Skip this many results
    #[clap(long, default_value = "0")]
    pub offset: usize,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DogetagSearchCommand {
    /// Text to search for in tag messages
    pub query: String,
    /// Maximum number of results
    #[clap(long, default_value = "50")]
    pub limit: usize,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DogetagAddressCommand {
    /// Dogecoin address to look up tags for
    pub address: String,
    /// Maximum number of results
    #[clap(long, default_value = "50")]
    pub limit: usize,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DogetagSendCommand {
    /// Recipient Dogecoin address
    #[clap(long)]
    pub to: String,
    /// Amount of DOGE to send (e.g. 5.0)
    #[clap(long)]
    pub amount: f64,
    /// Message to burn into the transaction OP_RETURN (max 80 bytes UTF-8)
    #[clap(long)]
    pub message: String,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

// ---------------------------------------------------------------------------
// DNS subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum DnsCommand {
    /// Resolve a Dogecoin Name System name (e.g. satoshi.doge)
    #[clap(name = "resolve")]
    Resolve(DnsResolveCommand),
    /// List registered DNS names
    #[clap(name = "list")]
    List(DnsListCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DnsResolveCommand {
    /// Name to resolve (e.g. satoshi.doge)
    pub name: String,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DnsListCommand {
    /// Filter by namespace (e.g. doge, shibe, kabosu)
    #[clap(long)]
    pub namespace: Option<String>,
    /// Maximum number of results
    #[clap(long, default_value = "100")]
    pub limit: usize,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

// ---------------------------------------------------------------------------
// Dogemap subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum DogemapCommand {
    /// Show claim status for a block number
    #[clap(name = "status")]
    Status(DogemapStatusCommand),
    /// List all claimed block numbers
    #[clap(name = "list")]
    List(DogemapListCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DogemapStatusCommand {
    /// Block number to query (e.g. 5056597)
    pub block_number: u32,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct DogemapListCommand {
    /// Maximum number of results
    #[clap(long, default_value = "100")]
    pub limit: usize,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

// ---------------------------------------------------------------------------
// doge-lotto subcommands
// ---------------------------------------------------------------------------

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum LottoCommand {
    /// Build a compact doge-lotto deploy inscription JSON payload
    #[clap(name = "deploy")]
    Deploy(LottoDeployCommand),
    /// Build, sign, and broadcast an atomic doge-lotto mint transaction
    #[clap(name = "mint")]
    Mint(LottoMintCommand),
    /// Show deployment and winner status for a lotto_id
    #[clap(name = "status")]
    Status(LottoStatusCommand),
    /// List indexed lottos
    #[clap(name = "list")]
    List(LottoListCommand),
    /// Transfer an expired ticket to the burn address to earn Burn Points
    #[clap(name = "burn")]
    Burn(LottoBurnCommand),
    /// Show Burn Points leaderboard
    #[clap(name = "burners")]
    Burners(LottoBurnersCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct LottoDeployCommand {
    /// Lotto identifier to deploy (e.g. doge-69-420, doge-max, my-mini-lotto-abc)
    #[clap(long = "type")]
    pub lotto_id: String,
    /// Future draw block height
    #[clap(long)]
    pub draw_block: u64,
    /// Optional ticket sales cutoff block (default: draw_block - 10)
    #[clap(long)]
    pub cutoff_block: Option<u64>,
    /// Ticket price in koinu
    #[clap(long)]
    pub ticket_price_koinu: u64,
    /// Prize pool address to receive ticket payments
    #[clap(long)]
    pub prize_pool_address: String,
    /// Fee percent for mini-lottos (0-10). doge-69-420 and doge-max must remain 0.
    #[clap(long, default_value = "0")]
    pub fee_percent: u8,
    /// Resolution mode: always_winner | closest_wins | exact_only_with_rollover
    #[clap(long)]
    pub resolution_mode: String,
    /// Optional template override (closest_wins, always_winner, custom, ...).
    /// Defaults to closest_wins unless preset type implies custom.
    #[clap(long)]
    pub template: Option<String>,
    /// Main drum pick count (defaults by lotto type).
    #[clap(long)]
    pub main_pick: Option<u16>,
    /// Main drum max number (defaults by lotto type).
    #[clap(long)]
    pub main_max: Option<u16>,
    /// Bonus drum pick count (0 disables; defaults by lotto type).
    #[clap(long)]
    pub bonus_pick: Option<u16>,
    /// Bonus drum max number (0 disables; defaults by lotto type).
    #[clap(long)]
    pub bonus_max: Option<u16>,
    /// Enable rollover when the resolution mode allows it
    #[clap(long)]
    pub rollover_enabled: bool,
    /// Optional guaranteed minimum prize in koinu
    #[clap(long)]
    pub guaranteed_min_prize_koinu: Option<u64>,
    /// Output as JSON wrapper instead of raw inscription payload
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct LottoMintCommand {
    /// Lotto identifier to mint against
    #[clap(long = "lotto")]
    pub lotto_id: String,
    /// Optional ticket id. Defaults to a generated id.
    #[clap(long)]
    pub ticket_id: Option<String>,
    /// Generate random unique numbers matching the deploy main drum config
    #[clap(long)]
    pub quickpick: bool,
    /// Comma-separated seed numbers. Must match deploy main drum pick/max constraints.
    #[clap(long)]
    pub seed_numbers: Option<String>,
    /// Optional immutable protocol developer tip percentage (0-10).
    #[clap(long, default_value = "0")]
    pub tip: u8,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output the broadcast result as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct LottoStatusCommand {
    /// Lotto id to query
    pub lotto_id: String,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
    /// Include payment verification details in the output
    #[clap(long)]
    pub show_payment_verification: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct LottoListCommand {
    /// Maximum number of results
    #[clap(long, default_value = "100")]
    pub limit: usize,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct LottoBurnCommand {
    /// Inscription ID of the ticket to burn (e.g. 1234567i0)
    pub ticket_inscription_id: String,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct LottoBurnersCommand {
    /// Maximum number of burners to show
    #[clap(long, default_value = "10")]
    pub limit: usize,
    /// Optional: show burn points for a specific address
    #[clap(long)]
    pub address: Option<String>,
    #[clap(long = "config-path")]
    pub config_path: String,
    /// Output as JSON
    #[clap(long)]
    pub json: bool,
}

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum Command {
    /// Stream and index Bitcoin blocks
    #[clap(subcommand)]
    Service(ServiceCommand),
    /// Perform maintenance operations on local index
    #[clap(subcommand)]
    Index(IndexCommand),
    /// Database operations
    #[clap(subcommand)]
    Database(DatabaseCommand),
}

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum DatabaseCommand {
    /// Migrates database
    #[clap(name = "migrate", bin_name = "migrate")]
    Migrate(MigrateDatabaseCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct MigrateDatabaseCommand {
    #[clap(long = "config-path")]
    pub config_path: String,
}

#[derive(Subcommand, PartialEq, Clone, Debug)]
#[clap(bin_name = "config", aliases = &["config"])]
pub enum ConfigCommand {
    /// Generate new config
    #[clap(name = "new", bin_name = "new", aliases = &["generate"])]
    New(NewConfigCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct NewConfigCommand {
    /// Target Regtest network
    #[clap(
        long = "regtest",
        conflicts_with = "testnet",
        conflicts_with = "mainnet"
    )]
    pub regtest: bool,
    /// Target Testnet network
    #[clap(
        long = "testnet",
        conflicts_with = "regtest",
        conflicts_with = "mainnet"
    )]
    pub testnet: bool,
    /// Target Mainnet network
    #[clap(
        long = "mainnet",
        conflicts_with = "testnet",
        conflicts_with = "regtest"
    )]
    pub mainnet: bool,
}

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum ServiceCommand {
    /// Start service
    #[clap(name = "start", bin_name = "start")]
    Start(ServiceStartCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct ServiceStartCommand {
    #[clap(long = "config-path")]
    pub config_path: String,
}

#[derive(Subcommand, PartialEq, Clone, Debug)]
pub enum IndexCommand {
    /// Sync index to latest bitcoin block
    #[clap(name = "sync", bin_name = "sync")]
    Sync(SyncIndexCommand),
    /// Rollback index blocks
    #[clap(name = "rollback", bin_name = "drop")]
    Rollback(RollbackIndexCommand),
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct SyncIndexCommand {
    #[clap(long = "config-path")]
    pub config_path: String,
}

#[derive(Parser, PartialEq, Clone, Debug)]
pub struct RollbackIndexCommand {
    /// Number of blocks to rollback from index tip
    pub blocks: u32,
    #[clap(long = "config-path")]
    pub config_path: String,
}
