use std::{
    path::PathBuf,
    process,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::sleep,
    time::Duration,
};

use dogecoin::{try_error, try_info, types::BlockIdentifier, utils::Context};
use dogecoin::bitcoincore_rpc::{
    bitcoin::{
        self, absolute::LockTime, hashes::Hash, Amount, OutPoint, ScriptBuf, Sequence, Transaction,
        TxIn, TxOut, Txid,
    },
    json::SignRawTransactionInput,
    RpcApi,
};
use clap::Parser;
use commands::{
    Command, ConfigCommand, DatabaseCommand, DnsCommand, DogemapCommand, IndexCommand,
    LottoCommand, Protocol, ServiceCommand,
};
use config::{generator::generate_toml_config, Config};
use hiro_system_kit;

mod commands;

const DEFAULT_LOTTO_FEE_RATE: f64 = 1.0;

pub fn main() {
    let logger = hiro_system_kit::log::setup_logger();
    let _guard = hiro_system_kit::log::setup_global_logger(logger.clone());
    let ctx = Context {
        logger: Some(logger),
        tracer: false,
    };

    let opts: Protocol = match Protocol::try_parse() {
        Ok(opts) => opts,
        Err(e) => {
            println!("{}", e);
            process::exit(1);
        }
    };

    if let Err(e) = hiro_system_kit::nestable_block_on(handle_command(opts, &ctx)) {
        try_error!(&ctx, "{e}");
        std::thread::sleep(std::time::Duration::from_millis(500));
        process::exit(1);
    }
}

fn check_maintenance_mode(ctx: &Context) {
    let maintenance_enabled = std::env::var("DOGHOOK_MAINTENANCE").unwrap_or("0".into());
    if maintenance_enabled.eq("1") {
        try_info!(
            ctx,
            "Entering maintenance mode. Unset DOGHOOK_MAINTENANCE and reboot to resume operations"
        );
        sleep(Duration::from_secs(u64::MAX))
    }
}

fn confirm_rollback(
    current_chain_tip: &BlockIdentifier,
    blocks_to_rollback: u32,
) -> Result<(), String> {
    println!("Index chain tip is at #{current_chain_tip}");
    println!(
        "{} blocks will be dropped. New index chain tip will be at #{}. Confirm? [Y/n]",
        blocks_to_rollback,
        current_chain_tip.index - blocks_to_rollback as u64
    );
    let mut buffer = String::new();
    std::io::stdin().read_line(&mut buffer).unwrap();
    if buffer.starts_with('n') {
        return Err("Deletion aborted".to_string());
    }
    Ok(())
}

async fn handle_command(opts: Protocol, ctx: &Context) -> Result<(), String> {
    // Set up the interrupt signal handler.
    let abort_signal = Arc::new(AtomicBool::new(false));
    let abort_signal_clone = abort_signal.clone();
    let ctx_moved = ctx.clone();
    ctrlc::set_handler(move || {
        try_info!(
            ctx_moved,
            "dogecoin-indexer received interrupt signal, shutting down..."
        );
        abort_signal_clone.store(true, Ordering::SeqCst);
    })
    .map_err(|e| format!("dogecoin-indexer failed to set interrupt signal handler: {e}"))?;

    match opts {
        Protocol::Doginals(subcmd) => match subcmd {
            Command::Service(subcmd) => match subcmd {
                ServiceCommand::Start(cmd) => {
                    check_maintenance_mode(ctx);
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_doginals_config()?;
                    doginals_indexer::start_doginals_indexer(true, &abort_signal, &config, ctx).await?
                }
            },
            Command::Index(index_command) => match index_command {
                IndexCommand::Sync(cmd) => {
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_doginals_config()?;
                    doginals_indexer::start_doginals_indexer(false, &abort_signal, &config, ctx).await?
                }
                IndexCommand::Rollback(cmd) => {
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_doginals_config()?;
                    let chain_tip = doginals_indexer::get_chain_tip(&config).await?;
                    confirm_rollback(&chain_tip, cmd.blocks)?;
                    doginals_indexer::rollback_block_range(
                        chain_tip.index - cmd.blocks as u64,
                        chain_tip.index,
                        &config,
                        ctx,
                    )
                    .await?;
                    println!("{} blocks dropped", cmd.blocks);
                }
            },
            Command::Database(database_command) => match database_command {
                DatabaseCommand::Migrate(cmd) => {
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_doginals_config()?;
                    doginals_indexer::db::migrate_dbs(&config, ctx).await?;
                }
            },
        },
        Protocol::Dunes(subcmd) => match subcmd {
            Command::Service(service_command) => match service_command {
                ServiceCommand::Start(cmd) => {
                    check_maintenance_mode(ctx);
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_dunes_config()?;
                    dunes::start_dunes_indexer(true, &abort_signal, &config, ctx).await?
                }
            },
            Command::Index(index_command) => match index_command {
                IndexCommand::Sync(cmd) => {
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_dunes_config()?;
                    dunes::start_dunes_indexer(false, &abort_signal, &config, ctx).await?
                }
                IndexCommand::Rollback(cmd) => {
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_dunes_config()?;
                    let chain_tip = dunes::get_chain_tip(&config).await?;
                    confirm_rollback(&chain_tip, cmd.blocks)?;
                    dunes::rollback_block_range(
                        chain_tip.index - cmd.blocks as u64,
                        chain_tip.index,
                        &config,
                        ctx,
                    )
                    .await?;
                    println!("{} blocks dropped", cmd.blocks);
                }
            },
            Command::Database(database_command) => match database_command {
                DatabaseCommand::Migrate(cmd) => {
                    let config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_dunes_config()?;
                    dunes::db::run_migrations(&config, ctx).await;
                }
            },
        },
        Protocol::Dns(subcmd) => match subcmd {
            DnsCommand::Resolve(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                match doginals_indexer::dns_resolve(&cmd.name, &config).await? {
                    Some(row) => {
                        if cmd.json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "name": row.name,
                                    "inscription_id": row.inscription_id,
                                    "block_height": row.block_height,
                                    "block_timestamp": row.block_timestamp,
                                })
                            );
                        } else {
                            println!("Name:           {}", row.name);
                            println!("Inscription ID: {}", row.inscription_id);
                            println!("Block Height:   {}", row.block_height);
                            println!("Timestamp:      {}", row.block_timestamp);
                        }
                    }
                    None => {
                        if cmd.json {
                            println!("null");
                        } else {
                            println!("Name not found: {}", cmd.name);
                        }
                        process::exit(1);
                    }
                }
            }
            DnsCommand::List(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                let (rows, total) =
                    doginals_indexer::dns_list(cmd.namespace.as_deref(), cmd.limit, 0, &config)
                        .await?;
                if cmd.json {
                    let json_rows: Vec<_> = rows
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "name": r.name,
                                "inscription_id": r.inscription_id,
                                "block_height": r.block_height,
                                "block_timestamp": r.block_timestamp,
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::json!({ "total": total, "names": json_rows })
                    );
                } else {
                    println!("DNS Names (Total: {total})");
                    println!("{:<40} {:<70} {}", "Name", "Inscription ID", "Height");
                    println!("{}", "-".repeat(115));
                    for row in &rows {
                        println!("{:<40} {:<70} {}", row.name, row.inscription_id, row.block_height);
                    }
                }
            }
        },
        Protocol::Dogemap(subcmd) => match subcmd {
            DogemapCommand::Status(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                match doginals_indexer::dogemap_status(cmd.block_number, &config).await? {
                    Some(row) => {
                        if cmd.json {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "block_number": row.block_number,
                                    "inscription_id": row.inscription_id,
                                    "claim_height": row.claim_height,
                                    "claim_timestamp": row.claim_timestamp,
                                })
                            );
                        } else {
                            println!("Block Number:   {}", row.block_number);
                            println!("Inscription ID: {}", row.inscription_id);
                            println!("Claim Height:   {}", row.claim_height);
                            println!("Timestamp:      {}", row.claim_timestamp);
                        }
                    }
                    None => {
                        if cmd.json {
                            println!("null");
                        } else {
                            println!("Block {} is unclaimed", cmd.block_number);
                        }
                        process::exit(1);
                    }
                }
            }
            DogemapCommand::List(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                let (rows, total) =
                    doginals_indexer::dogemap_list(cmd.limit, 0, &config).await?;
                if cmd.json {
                    let json_rows: Vec<_> = rows
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "block_number": r.block_number,
                                "inscription_id": r.inscription_id,
                                "claim_height": r.claim_height,
                                "claim_timestamp": r.claim_timestamp,
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::json!({ "total": total, "claims": json_rows })
                    );
                } else {
                    println!("Dogemap Claims (Total: {total})");
                    println!("{:<12} {:<70} {}", "Block", "Inscription ID", "Claim Height");
                    println!("{}", "-".repeat(95));
                    for row in &rows {
                        println!("{:<12} {:<70} {}", row.block_number, row.inscription_id, row.claim_height);
                    }
                }
            }
        },
        Protocol::Lotto(subcmd) => match subcmd {
            LottoCommand::Deploy(cmd) => {
                let resolution_mode = normalize_resolution_mode(&cmd.resolution_mode)?;
                if !(0..=10).contains(&cmd.fee_percent) {
                    return Err("fee_percent must be between 0 and 10".into());
                }
                if matches!(cmd.lotto_id.as_str(), "doge-69-420" | "doge-max") && cmd.fee_percent != 0 {
                    return Err(format!("{} must be deployed with fee_percent = 0", cmd.lotto_id));
                }
                let payload = serde_json::json!({
                    "p": "doge-lotto",
                    "op": "deploy",
                    "lotto_id": cmd.lotto_id,
                    "draw_block": cmd.draw_block,
                    "ticket_price_koinu": cmd.ticket_price_koinu,
                    "prize_pool_address": cmd.prize_pool_address,
                    "fee_percent": cmd.fee_percent,
                    "resolution_mode": resolution_mode,
                    "rollover_enabled": cmd.rollover_enabled,
                    "guaranteed_min_prize_koinu": cmd.guaranteed_min_prize_koinu,
                });
                let payload = compact_json_without_nulls(payload)?;
                if cmd.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "content_type": "text/plain",
                            "payload": payload,
                        })
                    );
                } else {
                    println!("{}", payload);
                }
            }
            LottoCommand::Mint(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                let Some(status) = doginals_indexer::lotto_status(&cmd.lotto_id, &config).await? else {
                    return Err(format!("Lotto not found: {}", cmd.lotto_id));
                };

                let seed_numbers = if let Some(seed_numbers) = &cmd.seed_numbers {
                    parse_seed_numbers_for_lotto(
                        seed_numbers,
                        status.summary.main_numbers_pick,
                        status.summary.main_numbers_max,
                    )?
                } else {
                    doginals_indexer::core::meta_protocols::lotto::quickpick_for_config(
                        &doginals_indexer::core::meta_protocols::lotto::NumberConfig {
                            pick: status.summary.main_numbers_pick,
                            max: status.summary.main_numbers_max,
                        },
                    )
                };
                let ticket_id = cmd
                    .ticket_id
                    .clone()
                    .unwrap_or_else(generate_ticket_id);

                let payload = serde_json::json!({
                    "p": "doge-lotto",
                    "op": "mint",
                    "lotto_id": cmd.lotto_id,
                    "ticket_id": ticket_id,
                    "seed_numbers": seed_numbers,
                });
                let payload = compact_json_without_nulls(payload)?;
                let result = broadcast_atomic_lotto_mint(
                    &config,
                    &status.summary.prize_pool_address,
                    status.summary.ticket_price_koinu,
                    &payload,
                    ctx,
                )?;

                if cmd.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "txid": result.txid.to_string(),
                            "inscription_id": format!("{}i0", result.txid),
                            "lotto_id": cmd.lotto_id,
                            "ticket_id": ticket_id,
                            "seed_numbers": seed_numbers,
                            "payload": payload,
                            "payment": {
                                "address": status.summary.prize_pool_address,
                                "amount_koinu": status.summary.ticket_price_koinu,
                            },
                            "fee_koinu": result.fee_koinu,
                            "change_koinu": result.change_koinu,
                        })
                    );
                } else {
                    println!("Broadcast txid:         {}", result.txid);
                    println!("Inscription ID:         {}i0", result.txid);
                    println!("Ticket ID:              {}", ticket_id);
                    println!("Payment Address:        {}", status.summary.prize_pool_address);
                    println!("Ticket Price (koinu):   {}", status.summary.ticket_price_koinu);
                    println!("Fee (koinu):            {}", result.fee_koinu);
                    println!("Change (koinu):         {}", result.change_koinu);
                }
            }
            LottoCommand::Status(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                match doginals_indexer::lotto_status(&cmd.lotto_id, &config).await? {
                    Some(row) => {
                        if cmd.json {
                            let winners: Vec<_> = row
                                .winners
                                .iter()
                                .map(|winner| {
                                    serde_json::json!({
                                        "lotto_id": winner.lotto_id,
                                        "inscription_id": winner.inscription_id,
                                        "ticket_id": winner.ticket_id,
                                        "resolved_height": winner.resolved_height,
                                        "rank": winner.rank,
                                        "score": winner.score,
                                        "payout_bps": winner.payout_bps,
                                        "payout_koinu": winner.payout_koinu,
                                        "seed_numbers": winner.seed_numbers,
                                        "drawn_numbers": winner.drawn_numbers,
                                    })
                                })
                                .collect();
                            println!(
                                "{}",
                                serde_json::json!({
                                    "lotto_id": row.summary.lotto_id,
                                    "inscription_id": row.summary.inscription_id,
                                    "deploy_height": row.summary.deploy_height,
                                    "deploy_timestamp": row.summary.deploy_timestamp,
                                    "draw_block": row.summary.draw_block,
                                    "ticket_price_koinu": row.summary.ticket_price_koinu,
                                    "prize_pool_address": row.summary.prize_pool_address,
                                    "fee_percent": row.summary.fee_percent,
                                    "resolution_mode": row.summary.resolution_mode,
                                    "rollover_enabled": row.summary.rollover_enabled,
                                    "guaranteed_min_prize_koinu": row.summary.guaranteed_min_prize_koinu,
                                    "resolved": row.summary.resolved,
                                    "resolved_height": row.summary.resolved_height,
                                    "verified_ticket_count": row.summary.verified_ticket_count,
                                    "verified_sales_koinu": row.summary.verified_sales_koinu,
                                    "net_prize_koinu": row.summary.net_prize_koinu,
                                    "rollover_occurred": row.summary.rollover_occurred,
                                    "current_ticket_count": row.summary.current_ticket_count,
                                    "winners": winners,
                                })
                            );
                        } else {
                            println!("Lotto ID:               {}", row.summary.lotto_id);
                            println!("Inscription ID:         {}", row.summary.inscription_id);
                            println!("Deploy Height:          {}", row.summary.deploy_height);
                            println!("Draw Block:             {}", row.summary.draw_block);
                            println!("Ticket Price (koinu):   {}", row.summary.ticket_price_koinu);
                            println!("Prize Pool Address:     {}", row.summary.prize_pool_address);
                            println!("Fee Percent:            {}", row.summary.fee_percent);
                            println!("Resolution Mode:        {}", row.summary.resolution_mode);
                            println!("Rollover Enabled:       {}", row.summary.rollover_enabled);
                            println!("Guaranteed Min Prize:   {}", row.summary.guaranteed_min_prize_koinu.map(|v| v.to_string()).unwrap_or_else(|| "-".into()));
                            println!("Current Ticket Count:   {}", row.summary.current_ticket_count);
                            println!("Resolved:               {}", row.summary.resolved);
                            println!("Resolved Height:        {}", row.summary.resolved_height.map(|v| v.to_string()).unwrap_or_else(|| "-".into()));
                            println!("Verified Ticket Count:  {}", row.summary.verified_ticket_count.map(|v| v.to_string()).unwrap_or_else(|| "-".into()));
                            println!("Verified Sales (koinu): {}", row.summary.verified_sales_koinu.map(|v| v.to_string()).unwrap_or_else(|| "-".into()));
                            println!("Net Prize (koinu):      {}", row.summary.net_prize_koinu.map(|v| v.to_string()).unwrap_or_else(|| "-".into()));
                            println!("Rollover Occurred:      {}", row.summary.rollover_occurred);
                            if row.winners.is_empty() {
                                println!("Winners:                none");
                            } else {
                                println!("Winners:");
                                for winner in &row.winners {
                                    println!(
                                        "  rank {} ticket {} payout {} koinu score {} inscription {}",
                                        winner.rank,
                                        winner.ticket_id,
                                        winner.payout_koinu,
                                        winner.score,
                                        winner.inscription_id
                                    );
                                }
                            }
                        }
                    }
                    None => {
                        if cmd.json {
                            println!("null");
                        } else {
                            println!("Lotto not found: {}", cmd.lotto_id);
                        }
                        process::exit(1);
                    }
                }
            }
            LottoCommand::List(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                let (rows, total) = doginals_indexer::lotto_list(cmd.limit, 0, &config).await?;
                if cmd.json {
                    let json_rows: Vec<_> = rows
                        .iter()
                        .map(|row| {
                            serde_json::json!({
                                "lotto_id": row.lotto_id,
                                "inscription_id": row.inscription_id,
                                "deploy_height": row.deploy_height,
                                "draw_block": row.draw_block,
                                "ticket_price_koinu": row.ticket_price_koinu,
                                "prize_pool_address": row.prize_pool_address,
                                "fee_percent": row.fee_percent,
                                "resolution_mode": row.resolution_mode,
                                "resolved": row.resolved,
                                "resolved_height": row.resolved_height,
                                "current_ticket_count": row.current_ticket_count,
                                "verified_ticket_count": row.verified_ticket_count,
                                "net_prize_koinu": row.net_prize_koinu,
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::json!({ "total": total, "lottos": json_rows })
                    );
                } else {
                    println!("doge-lotto Deployments (Total: {total})");
                    println!(
                        "{:<24} {:<12} {:<10} {:<8} {:<8} {}",
                        "Lotto ID", "Draw Block", "Tickets", "Fee %", "Resolved", "Mode"
                    );
                    println!("{}", "-".repeat(78));
                    for row in &rows {
                        println!(
                            "{:<24} {:<12} {:<10} {:<8} {:<8} {}",
                            row.lotto_id,
                            row.draw_block,
                            row.current_ticket_count,
                            row.fee_percent,
                            row.resolved,
                            row.resolution_mode,
                        );
                    }
                }
            }
        },
        Protocol::Config(subcmd) => match subcmd {
            ConfigCommand::New(cmd) => {
                use std::{fs::File, io::Write};
                let network = match (cmd.mainnet, cmd.testnet, cmd.regtest) {
                    (true, false, false) => "mainnet",
                    (false, true, false) => "testnet",
                    (false, false, true) => "regtest",
                    _ => return Err("Invalid network".into()),
                };
                let config_content = generate_toml_config(network);
                let mut file_path = PathBuf::new();
                file_path.push("Indexer.toml");
                let mut file = File::create(&file_path)
                    .map_err(|e| format!("unable to open file {}\n{}", file_path.display(), e))?;
                file.write_all(config_content.as_bytes())
                    .map_err(|e| format!("unable to write file {}\n{}", file_path.display(), e))?;
                println!("Created file Indexer.toml");
            }
        },
    }
    Ok(())
}

fn normalize_resolution_mode(value: &str) -> Result<&'static str, String> {
    match value {
        "always_winner" => Ok("always_winner"),
        "closest_wins" => Ok("closest_wins"),
        "exact_only_with_rollover" => Ok("exact_only_with_rollover"),
        _ => Err(format!(
            "invalid resolution mode: {} (expected always_winner, closest_wins, or exact_only_with_rollover)",
            value
        )),
    }
}

fn compact_json_without_nulls(mut value: serde_json::Value) -> Result<String, String> {
    prune_nulls(&mut value);
    serde_json::to_string(&value).map_err(|e| format!("unable to serialize lotto payload: {e}"))
}

fn prune_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|_, inner| {
                prune_nulls(inner);
                !inner.is_null()
            });
        }
        serde_json::Value::Array(values) => {
            for inner in values {
                prune_nulls(inner);
            }
        }
        _ => {}
    }
}

fn parse_seed_numbers_for_lotto(
    value: &str,
    expected_pick: u16,
    max_number: u16,
) -> Result<Vec<u16>, String> {
    let mut parsed: Vec<u16> = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<u16>()
                .map_err(|e| format!("invalid seed number '{}': {}", part, e))
        })
        .collect::<Result<_, _>>()?;

    if parsed.len() != expected_pick as usize {
        return Err(format!(
            "seed_numbers must contain exactly {} unique values in [1, {}]",
            expected_pick, max_number
        ));
    }

    parsed.sort_unstable();
    parsed.dedup();
    if parsed.len() != expected_pick as usize
        || parsed
            .iter()
            .any(|number| !(1..=max_number).contains(number))
    {
        return Err(format!(
            "seed_numbers must contain exactly {} unique values in [1, {}]",
            expected_pick, max_number
        ));
    }

    Ok(parsed)
}

fn generate_ticket_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("ticket-{}-{}", now.as_secs(), now.subsec_nanos())
}

struct AtomicLottoMintResult {
    txid: Txid,
    fee_koinu: u64,
    change_koinu: u64,
}

fn broadcast_atomic_lotto_mint(
    config: &Config,
    prize_pool_address: &str,
    ticket_price_koinu: u64,
    payload: &str,
    ctx: &Context,
) -> Result<AtomicLottoMintResult, String> {
    let client = dogecoin::utils::bitcoind::dogecoin_get_client(&config.dogecoin, ctx);
    let script_segments = build_lotto_inscription_segments(payload.as_bytes());
    let output_count = if ticket_price_koinu > 0 { 2 } else { 1 };
    let fee_koinu = calc_lotto_fee(script_sig_size(&script_segments), output_count, DEFAULT_LOTTO_FEE_RATE);
    let required_koinu = ticket_price_koinu.saturating_add(fee_koinu);
    let (funding_txid, funding_vout, funding_value, funding_script) =
        select_lotto_utxo(&client, required_koinu, ticket_price_koinu == 0)?;

    let funding_koinu = funding_value.to_sat();
    let change_koinu = funding_koinu.saturating_sub(required_koinu);
    if ticket_price_koinu == 0 && change_koinu == 0 {
        return Err(
            "free lotto mint requires a wallet UTXO larger than the estimated fee so the transaction can keep one standard output".into(),
        );
    }

    let mut outputs = Vec::new();
    if ticket_price_koinu > 0 {
        outputs.push(TxOut {
            value: Amount::from_sat(ticket_price_koinu),
            script_pubkey: parse_dogecoin_address(prize_pool_address)?,
        });
    }
    if change_koinu > 0 || ticket_price_koinu == 0 {
        let change_address: String = client
            .call("getrawchangeaddress", &[])
            .map_err(|e| format!("unable to get raw change address: {e}"))?;
        let change_value = if ticket_price_koinu == 0 {
            funding_koinu.saturating_sub(fee_koinu)
        } else {
            change_koinu
        };
        outputs.push(TxOut {
            value: Amount::from_sat(change_value),
            script_pubkey: parse_dogecoin_address(&change_address)?,
        });
    }

    let template = Transaction {
        version: bitcoin::transaction::Version(1),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: funding_txid,
                vout: funding_vout,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }],
        output: outputs.clone(),
    };

    let (sig_bytes, pubkey_bytes) = sign_lotto_template(
        &client,
        &template,
        funding_txid,
        funding_vout,
        &funding_script,
        funding_value,
    )?;

    let final_tx = Transaction {
        version: bitcoin::transaction::Version(1),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: funding_txid,
                vout: funding_vout,
            },
            script_sig: ScriptBuf::from(build_script_sig(
                &script_segments,
                &sig_bytes,
                &pubkey_bytes,
            )),
            sequence: Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }],
        output: outputs,
    };

    let txid = client
        .send_raw_transaction(&final_tx)
        .map_err(|e| format!("unable to broadcast atomic lotto mint transaction: {e}"))?;

    Ok(AtomicLottoMintResult {
        txid,
        fee_koinu,
        change_koinu: if ticket_price_koinu == 0 {
            funding_koinu.saturating_sub(fee_koinu)
        } else {
            change_koinu
        },
    })
}

fn build_lotto_inscription_segments(payload: &[u8]) -> Vec<Vec<u8>> {
    vec![
        encode_push(b"ord"),
        encode_push(&1_u16.to_le_bytes()),
        encode_push(b"text/plain"),
        encode_count(0),
        encode_push(payload),
    ]
}

fn encode_push(data: &[u8]) -> Vec<u8> {
    let len = data.len();
    let mut out = Vec::with_capacity(len + 3);
    if len == 0 {
        out.push(0x00);
    } else if len <= 75 {
        out.push(len as u8);
        out.extend_from_slice(data);
    } else if len <= 255 {
        out.push(0x4c);
        out.push(len as u8);
        out.extend_from_slice(data);
    } else {
        out.push(0x4d);
        out.push((len & 0xff) as u8);
        out.push((len >> 8) as u8);
        out.extend_from_slice(data);
    }
    out
}

fn encode_push_size(data_len: usize) -> usize {
    if data_len == 0 {
        1
    } else if data_len <= 75 {
        1 + data_len
    } else if data_len <= 255 {
        2 + data_len
    } else {
        3 + data_len
    }
}

fn encode_count(n: u16) -> Vec<u8> {
    match n {
        0 => vec![0x00],
        1..=16 => vec![0x50 + n as u8],
        17..=127 => vec![0x01, n as u8],
        _ => vec![0x02, (n & 0xff) as u8, (n >> 8) as u8],
    }
}

fn build_script_sig(segments: &[Vec<u8>], sig: &[u8], pubkey: &[u8]) -> Vec<u8> {
    let total = segments.iter().map(|segment| segment.len()).sum::<usize>()
        + encode_push_size(sig.len())
        + encode_push_size(pubkey.len());
    let mut out = Vec::with_capacity(total);
    for segment in segments {
        out.extend_from_slice(segment);
    }
    out.extend(encode_push(sig));
    out.extend(encode_push(pubkey));
    out
}

fn script_sig_size(segments: &[Vec<u8>]) -> usize {
    let segments_size: usize = segments.iter().map(|segment| segment.len()).sum();
    segments_size + 75 + 34
}

fn calc_lotto_fee(script_sig_bytes: usize, n_outputs: usize, fee_rate: f64) -> u64 {
    let script_varint = if script_sig_bytes < 0xfd { 1usize } else { 3 };
    let input_size = 32 + 4 + script_varint + script_sig_bytes + 4;
    let output_size = n_outputs * (8 + 1 + 25);
    let total_size = 10 + input_size + output_size;
    (total_size as f64 * fee_rate).ceil() as u64
}

fn select_lotto_utxo(
    client: &dogecoin::bitcoincore_rpc::Client,
    required_koinu: u64,
    require_strictly_more: bool,
) -> Result<(Txid, u32, Amount, ScriptBuf), String> {
    let utxos = client
        .list_unspent(Some(1), None, None, None, None)
        .map_err(|e| format!("unable to list wallet UTXOs: {e}"))?;

    utxos
        .into_iter()
        .filter(|utxo| {
            utxo.spendable
                && if require_strictly_more {
                    utxo.amount.to_sat() > required_koinu
                } else {
                    utxo.amount.to_sat() >= required_koinu
                }
        })
        .max_by_key(|utxo| utxo.amount)
        .map(|utxo| (utxo.txid, utxo.vout, utxo.amount, utxo.script_pub_key))
        .ok_or_else(|| {
            format!(
                "no spendable wallet UTXO covers {} koinu required for the atomic lotto mint",
                required_koinu
            )
        })
}

fn sign_lotto_template(
    client: &dogecoin::bitcoincore_rpc::Client,
    tx: &Transaction,
    input_txid: Txid,
    input_vout: u32,
    input_script: &ScriptBuf,
    input_value: Amount,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let result = client
        .sign_raw_transaction_with_wallet(
            tx,
            Some(&[SignRawTransactionInput {
                txid: input_txid,
                vout: input_vout,
                script_pub_key: input_script.clone(),
                redeem_script: None,
                amount: Some(input_value),
            }]),
            None,
        )
        .map_err(|e| format!("wallet failed to sign lotto mint template: {e}"))?;

    if !result.complete {
        return Err(format!(
            "wallet could not fully sign the lotto mint transaction: {:?}",
            result.errors
        ));
    }

    let signed_tx: Transaction = bitcoin::consensus::deserialize(&result.hex)
        .map_err(|e| format!("unable to decode signed transaction: {e}"))?;
    let script_sig = &signed_tx.input[0].script_sig;
    let mut instructions = script_sig.instructions();

    let sig = match instructions.next() {
        Some(Ok(bitcoin::script::Instruction::PushBytes(push_bytes))) => push_bytes.as_bytes().to_vec(),
        other => {
            return Err(format!(
                "unexpected signature instruction in signed lotto mint input: {:?}",
                other
            ))
        }
    };

    let pubkey = match instructions.next() {
        Some(Ok(bitcoin::script::Instruction::PushBytes(push_bytes))) => push_bytes.as_bytes().to_vec(),
        other => {
            return Err(format!(
                "unexpected pubkey instruction in signed lotto mint input: {:?}",
                other
            ))
        }
    };

    Ok((sig, pubkey))
}

fn parse_dogecoin_address(addr: &str) -> Result<ScriptBuf, String> {
    let decoded = bitcoin::base58::decode_check(addr)
        .map_err(|e| format!("invalid Dogecoin address '{}': {}", addr, e))?;
    if decoded.is_empty() {
        return Err(format!("invalid Dogecoin address '{}': empty payload", addr));
    }

    let version = decoded[0];
    let payload = &decoded[1..];
    match version {
        0x1e => {
            let hash = bitcoin::PubkeyHash::from_slice(payload)
                .map_err(|e| format!("invalid P2PKH address '{}': {}", addr, e))?;
            Ok(ScriptBuf::new_p2pkh(&hash))
        }
        0x16 => {
            let hash = bitcoin::ScriptHash::from_slice(payload)
                .map_err(|e| format!("invalid P2SH address '{}': {}", addr, e))?;
            Ok(ScriptBuf::new_p2sh(&hash))
        }
        _ => Err(format!("unsupported Dogecoin address version '{}' for '{}'", version, addr)),
    }
}
