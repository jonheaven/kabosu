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

use clap::Parser;
use commands::{
    Command, ConfigCommand, DatabaseCommand, DnsCommand, DogemapCommand, DogetagCommand,
    DogetagSendCommand, IndexCommand, LottoCommand, Protocol, RefreshBlkIndexCommand,
    ScanIndexCommand, ServiceCommand,
};
use config::{
    generator::generate_toml_config, Config, DogecoinDataSource, DoginalsPredicatesConfig,
};
use dogecoin::bitcoincore_rpc::{
    bitcoin::{
        self, absolute::LockTime, hashes::Hash, Amount, OutPoint, ScriptBuf, Sequence, Transaction,
        TxIn, TxOut, Txid,
    },
    json::SignRawTransactionInput,
    RpcApi,
};
use dogecoin::{try_error, try_info, types::BlockIdentifier, utils::Context};
use hiro_system_kit;
use postgres::pg_pool;

mod commands;

const DEFAULT_LOTTO_FEE_RATE: f64 = 1.0;
const ONLY_PREDICATE_SENTINEL_MIME: &str = "application/x-kabosu-never-index";

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

fn run_refresh_blk_index(cmd: &RefreshBlkIndexCommand, ctx: &Context) -> Result<(), String> {
    use dogecoin::blk_reader::refresh_index_copy;

    let config = Config::from_file_path(&cmd.config_path)?;
    let data_dir = config.dogecoin.dogecoin_data_dir.ok_or_else(|| {
        "dogecoin.dogecoin_data_dir is not set in the config file.\n\
         Add dogecoin_data_dir = \"/path/to/.dogecoin\" under [dogecoin] to enable direct .blk reads."
            .to_string()
    })?;

    let live_index = PathBuf::from(&data_dir).join("blocks").join("index");
    if !live_index.exists() {
        eprintln!("Block index not found at {}", live_index.display());
        eprintln!("Is Dogecoin Core installed and has it completed the initial block download?");
        return Err(format!("block index not found at {}", live_index.display()));
    }

    let copy_dir = config
        .dogecoin
        .blk_index_copy_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&config.storage.working_dir).join("blk-index"));

    println!("Refreshing block index copy...");
    println!("  Source: {}", live_index.display());
    println!("  Dest:   {}", copy_dir.display());

    try_info!(
        ctx,
        "Refreshing blk index: {} → {}",
        live_index.display(),
        copy_dir.display()
    );
    let (copied, skipped) = refresh_index_copy(&live_index, &copy_dir)?;

    println!("Done: {copied} file(s) updated, {skipped} already up-to-date.");
    println!("kabosu will now use direct .blk reads on the next sync (5-20× faster).");
    try_info!(
        ctx,
        "blk-index refresh: {copied} updated, {skipped} unchanged → {}",
        copy_dir.display()
    );
    Ok(())
}

async fn run_doginals_scan(
    cmd: ScanIndexCommand,
    abort_signal: &Arc<AtomicBool>,
    ctx: &Context,
) -> Result<(), String> {
    use doginals_indexer::{scan_doginals, ScanOptions};
    use std::fs::File;
    use std::io::{stdout, BufWriter};

    let config = Config::from_file_path(&cmd.config_path)?;

    if cmd.from > cmd.to {
        return Err(format!(
            "--from ({}) must be <= --to ({})",
            cmd.from, cmd.to
        ));
    }

    // --predicate overrides --content-type when both are given.
    let content_type_filter = if let Some(ref pred) = cmd.predicate {
        if let Some(suffix) = pred.strip_prefix("mime:") {
            Some(suffix.to_string())
        } else {
            return Err(format!(
                "unsupported predicate '{}' — supported formats: mime:<prefix>",
                pred
            ));
        }
    } else {
        cmd.content_type.clone()
    };

    let opts = ScanOptions {
        reveals_only: cmd.reveals_only,
        content_type_prefix: content_type_filter,
    };

    try_info!(
        ctx,
        "Scanning blocks {} to {} for inscriptions{}{}...",
        cmd.from,
        cmd.to,
        cmd.reveals_only.then_some(" (reveals only)").unwrap_or(""),
        cmd.content_type
            .as_deref()
            .map(|p| format!(" [content-type: {p}]"))
            .unwrap_or_default()
    );

    let count = if let Some(out_path) = &cmd.out {
        let file = File::create(out_path)
            .map_err(|e| format!("cannot create output file {out_path}: {e}"))?;
        let writer = Arc::new(std::sync::Mutex::new(BufWriter::new(file)));
        let n = scan_doginals(cmd.from, cmd.to, opts, writer, abort_signal, &config, ctx).await?;
        try_info!(ctx, "Scan complete: {n} events written to {out_path}");
        n
    } else {
        // Stream directly to stdout.
        let writer = Arc::new(std::sync::Mutex::new(BufWriter::new(stdout())));
        scan_doginals(cmd.from, cmd.to, opts, writer, abort_signal, &config, ctx).await?
    };

    eprintln!(
        "Scan complete: {count} inscription event(s) in blocks {}..{}.",
        cmd.from, cmd.to
    );
    Ok(())
}

fn check_maintenance_mode(ctx: &Context) {
    let maintenance_enabled = std::env::var("KABOSU_MAINTENANCE").unwrap_or("0".into());
    if maintenance_enabled.eq("1") {
        try_info!(
            ctx,
            "Entering maintenance mode. Unset KABOSU_MAINTENANCE and reboot to resume operations"
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

/// Parse a block range string like "500000..500100" into `(start, end)`.
fn parse_blk_range(s: &str) -> Result<(u64, u64), String> {
    let parts: Vec<&str> = s.splitn(2, "..").collect();
    if parts.len() != 2 {
        return Err(format!(
            "--test-blk-range '{}': expected format START..END (e.g. 500000..500100)",
            s
        ));
    }
    let start = parts[0]
        .parse::<u64>()
        .map_err(|_| format!("--test-blk-range: invalid start block '{}'", parts[0]))?;
    let end = parts[1]
        .parse::<u64>()
        .map_err(|_| format!("--test-blk-range: invalid end block '{}'", parts[1]))?;
    if start > end {
        return Err(format!(
            "--test-blk-range: start ({start}) must be <= end ({end})"
        ));
    }
    Ok((start, end))
}

fn apply_only_protocol_selection(
    config: &mut Config,
    only: Option<&str>,
    ctx: &Context,
) -> Result<(), String> {
    let Some(only) = only else {
        return Ok(());
    };

    let only = only.trim().to_ascii_lowercase();

    config.protocols.dns.enabled = false;
    config.protocols.dogemap.enabled = false;
    config.protocols.dogetag.enabled = false;
    config.protocols.lotto.enabled = false;
    config.protocols.dogespells.enabled = false;
    config.protocols.dmp.enabled = false;

    if let Some(doginals) = config.doginals.as_mut() {
        doginals.predicates = Some(DoginalsPredicatesConfig {
            enabled: true,
            mime_types: vec![ONLY_PREDICATE_SENTINEL_MIME.to_string()],
            content_prefixes: vec![],
        });

        if let Some(meta_protocols) = doginals.meta_protocols.as_mut() {
            if let Some(drc20) = meta_protocols.drc20.as_mut() {
                drc20.enabled = false;
            }
        }
    }

    match only.as_str() {
        "dns" => config.protocols.dns.enabled = true,
        "dogemap" => config.protocols.dogemap.enabled = true,
        "dogetag" => config.protocols.dogetag.enabled = true,
        "dogelotto" | "lotto" => config.protocols.lotto.enabled = true,
        "dogespells" => config.protocols.dogespells.enabled = true,
        "dmp" => config.protocols.dmp.enabled = true,
        other => {
            return Err(format!(
                "unsupported --only value '{}' (expected one of: dns, dogemap, dogetag, dogelotto, dogespells, dmp)",
                other
            ));
        }
    }

    try_info!(
        ctx,
        "--only {}: indexing only that metaprotocol and skipping inscription storage for this run",
        only
    );

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
                    let mut config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_doginals_config()?;
                    apply_only_protocol_selection(&mut config, cmd.only.as_deref(), ctx)?;

                    // Start web explorer if enabled
                    let web_enabled = config.web.as_ref().map(|w| w.enabled).unwrap_or(false);
                    let web_port = config.web.as_ref().map(|w| w.port).unwrap_or(8080);
                    if web_enabled {
                        let doginals_config = config.doginals.as_ref().unwrap();
                        let doginals_pool = Arc::new(
                            pg_pool(&doginals_config.db)
                                .map_err(|e| format!("Failed to create doginals pool: {}", e))?,
                        );

                        let drc20_pool = config
                            .ordinals_drc20_config()
                            .map(|drc20| pg_pool(&drc20.db))
                            .transpose()
                            .map_err(|e| format!("Failed to create DRCC-20 pool: {}", e))?
                            .map(Arc::new);

                        let dunes_pool = config
                            .dunes
                            .as_ref()
                            .map(|dunes| pg_pool(&dunes.db))
                            .transpose()
                            .map_err(|e| format!("Failed to create Dunes pool: {}", e))?
                            .map(Arc::new);

                        let web_addr = format!("0.0.0.0:{}", web_port)
                            .parse()
                            .map_err(|e| format!("Invalid web server address: {}", e))?;
                        let burn_address = config.protocols.lotto.burn_address.clone();

                        // Auto-register the local webhook URL so every indexed event is
                        // fanned out to /api/events SSE subscribers (e.g. dogecoin.games).
                        let local_webhook = format!("http://127.0.0.1:{}/api/webhook", web_port);
                        if !config.webhooks.urls.contains(&local_webhook) {
                            config.webhooks.urls.push(local_webhook);
                        }

                        let dogecoin_config = config.dogecoin.clone();
                        tokio::spawn(async move {
                            if let Err(e) = crate::web::start_web_server(
                                web_addr,
                                doginals_pool,
                                drc20_pool,
                                dunes_pool,
                                burn_address,
                                dogecoin_config,
                            )
                            .await
                            {
                                eprintln!("Web server error: {}", e);
                            }
                        });
                    }

                    doginals_indexer::start_doginals_indexer(true, &abort_signal, &config, ctx)
                        .await?
                }
            },
            Command::Index(index_command) => match index_command {
                IndexCommand::Sync(cmd) => {
                    let mut config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_doginals_config()?;
                    apply_only_protocol_selection(&mut config, cmd.only.as_deref(), ctx)?;
                    if let Some(from) = cmd.from {
                        config.start_block = Some(from);
                    }
                    if let Some(to) = cmd.to {
                        config.stop_block = Some(to);
                    }
                    if let Some(range_str) = &cmd.test_blk_range {
                        let (start, end) = parse_blk_range(range_str)?;
                        config.dogecoin.data_source = DogecoinDataSource::File;
                        config.start_block = Some(start);
                        config.stop_block = Some(end);
                        try_info!(
                            ctx,
                            "--test-blk-range: forcing file mode, blocks {}..{}",
                            start,
                            end
                        );
                    }
                    doginals_indexer::start_doginals_indexer(false, &abort_signal, &config, ctx)
                        .await?
                }
                IndexCommand::Scan(cmd) => {
                    run_doginals_scan(cmd, &abort_signal, ctx).await?;
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
                IndexCommand::RefreshBlkIndex(cmd) => {
                    run_refresh_blk_index(&cmd, ctx)?;
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
                    let mut config = Config::from_file_path(&cmd.config_path)?;
                    config.assert_dunes_config()?;
                    if let Some(from) = cmd.from {
                        config.start_block = Some(from);
                    }
                    if let Some(to) = cmd.to {
                        config.stop_block = Some(to);
                    }
                    if let Some(range_str) = &cmd.test_blk_range {
                        let (start, end) = parse_blk_range(range_str)?;
                        config.dogecoin.data_source = DogecoinDataSource::File;
                        config.start_block = Some(start);
                        config.stop_block = Some(end);
                        try_info!(
                            ctx,
                            "--test-blk-range: forcing file mode, blocks {}..{}",
                            start,
                            end
                        );
                    }
                    dunes::start_dunes_indexer(false, &abort_signal, &config, ctx).await?
                }
                IndexCommand::Scan(_) => {
                    return Err("scan is only available under `doginals index scan`".to_string());
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
                IndexCommand::RefreshBlkIndex(cmd) => {
                    run_refresh_blk_index(&cmd, ctx)?;
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
                        println!(
                            "{:<40} {:<70} {}",
                            row.name, row.inscription_id, row.block_height
                        );
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
                let (rows, total) = doginals_indexer::dogemap_list(cmd.limit, 0, &config).await?;
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
                    println!(
                        "{:<12} {:<70} {}",
                        "Block", "Inscription ID", "Claim Height"
                    );
                    println!("{}", "-".repeat(95));
                    for row in &rows {
                        println!(
                            "{:<12} {:<70} {}",
                            row.block_number, row.inscription_id, row.claim_height
                        );
                    }
                }
            }
        },
        Protocol::Lotto(subcmd) => match subcmd {
            LottoCommand::Deploy(cmd) => {
                let resolution_mode = normalize_resolution_mode(&cmd.resolution_mode)?;
                if cmd.draw_block <= 10 && cmd.cutoff_block.is_none() {
                    return Err(
                        "draw_block must be > 10 when cutoff_block is omitted (default cutoff is draw_block - 10)"
                            .into(),
                    );
                }
                let cutoff_block = cmd
                    .cutoff_block
                    .unwrap_or_else(|| cmd.draw_block.saturating_sub(10));
                if cutoff_block == 0 || cutoff_block >= cmd.draw_block {
                    return Err("cutoff_block must be > 0 and strictly less than draw_block".into());
                }
                if !(0..=10).contains(&cmd.fee_percent) {
                    return Err("fee_percent must be between 0 and 10".into());
                }
                if matches!(cmd.lotto_id.as_str(), "doge-69-420" | "doge-max")
                    && cmd.fee_percent != 0
                {
                    return Err(format!(
                        "{} must be deployed with fee_percent = 0",
                        cmd.lotto_id
                    ));
                }

                let defaults = lotto_number_defaults(&cmd.lotto_id);
                let main_numbers = serde_json::json!({
                    "pick": cmd.main_pick.unwrap_or(defaults.main_pick),
                    "max": cmd.main_max.unwrap_or(defaults.main_max),
                });
                let bonus_numbers = serde_json::json!({
                    "pick": cmd.bonus_pick.unwrap_or(defaults.bonus_pick),
                    "max": cmd.bonus_max.unwrap_or(defaults.bonus_max),
                });
                let template = if let Some(template) = &cmd.template {
                    normalize_template(template)?
                } else {
                    defaults.template
                };

                let payload = serde_json::json!({
                    "p": "DogeLotto",
                    "op": "deploy",
                    "lotto_id": cmd.lotto_id,
                    "template": template,
                    "draw_block": cmd.draw_block,
                    "cutoff_block": cutoff_block,
                    "ticket_price_koinu": cmd.ticket_price_koinu,
                    "prize_pool_address": cmd.prize_pool_address,
                    "fee_percent": cmd.fee_percent,
                    "main_numbers": main_numbers,
                    "bonus_numbers": bonus_numbers,
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
                if !(0..=10).contains(&cmd.tip) {
                    return Err("tip must be between 0 and 10".into());
                }
                if cmd.tip > 0
                    && config
                        .protocols
                        .lotto
                        .protocol_dev_address
                        .trim()
                        .is_empty()
                {
                    return Err(
                        "protocols.lotto.protocol_dev_address must be set when using --tip".into(),
                    );
                }
                let Some(status) = doginals_indexer::lotto_status(&cmd.lotto_id, &config).await?
                else {
                    return Err(format!("Lotto not found: {}", cmd.lotto_id));
                };
                let chain_tip =
                    dogecoin::utils::bitcoind::dogecoin_get_chain_tip(&config.dogecoin, ctx);
                if chain_tip.index > status.summary.cutoff_block {
                    return Err(format!(
                        "ticket sales closed for {} at block {} (current tip #{})",
                        cmd.lotto_id, status.summary.cutoff_block, chain_tip.index
                    ));
                }

                let seed_numbers = if let Some(seed_numbers) = &cmd.seed_numbers {
                    parse_seed_numbers_for_lotto(
                        seed_numbers,
                        status.summary.main_numbers_pick,
                        status.summary.main_numbers_max,
                    )?
                } else if cmd.quickpick {
                    doginals_indexer::core::meta_protocols::lotto::quickpick_for_config(
                        &doginals_indexer::core::meta_protocols::lotto::NumberConfig {
                            pick: status.summary.main_numbers_pick,
                            max: status.summary.main_numbers_max,
                        },
                    )
                } else {
                    return Err("lotto mint requires either --quickpick or --seed-numbers".into());
                };
                let ticket_id = cmd.ticket_id.clone().unwrap_or_else(generate_ticket_id);
                let is_deno = cmd.lotto_id == "deno";

                let payload = serde_json::json!({
                    "p": "DogeLotto",
                    "op": "mint",
                    "lotto_id": cmd.lotto_id,
                    "ticket_id": ticket_id,
                    "seed_numbers": if is_deno { serde_json::Value::Null } else { serde_json::json!(seed_numbers.clone()) },
                    "luck_marks": if is_deno { serde_json::json!(seed_numbers.clone()) } else { serde_json::Value::Null },
                    "tip_percent": cmd.tip,
                });
                let payload = compact_json_without_nulls(payload)?;
                let result = broadcast_atomic_lotto_mint(
                    &config,
                    &status.summary.prize_pool_address,
                    status.summary.ticket_price_koinu,
                    cmd.tip,
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
                            "luck_marks": if is_deno { Some(seed_numbers.clone()) } else { None::<Vec<u16>> },
                            "payload": payload,
                            "payment": {
                                "address": status.summary.prize_pool_address,
                                "amount_koinu": status.summary.ticket_price_koinu,
                            },
                            "tip_percent": cmd.tip,
                            "tip_koinu": result.tip_koinu,
                            "protocol_dev_address": config.protocols.lotto.protocol_dev_address,
                            "fee_koinu": result.fee_koinu,
                            "change_koinu": result.change_koinu,
                        })
                    );
                } else {
                    println!("Broadcast txid:         {}", result.txid);
                    println!("Inscription ID:         {}i0", result.txid);
                    println!("Ticket ID:              {}", ticket_id);
                    println!(
                        "Payment Address:        {}",
                        status.summary.prize_pool_address
                    );
                    println!(
                        "Ticket Price (koinu):   {}",
                        status.summary.ticket_price_koinu
                    );
                    println!("Tip Percent:            {}", cmd.tip);
                    println!("Tip Amount (koinu):     {}", result.tip_koinu);
                    if cmd.tip > 0 {
                        println!(
                            "Protocol Dev Address:   {}",
                            config.protocols.lotto.protocol_dev_address
                        );
                    }
                    println!("Fee (koinu):            {}", result.fee_koinu);
                    println!("Change (koinu):         {}", result.change_koinu);
                }
            }
            LottoCommand::Status(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;
                let chain_tip =
                    dogecoin::utils::bitcoind::dogecoin_get_chain_tip(&config.dogecoin, ctx);
                match doginals_indexer::lotto_status(&cmd.lotto_id, &config).await? {
                    Some(row) => {
                        let blocks_remaining =
                            row.summary.cutoff_block.saturating_sub(chain_tip.index);
                        // Calculate days since draw for unclaimed prize policy (1 block ≈ 1 minute)
                        let days_since_draw =
                            if let Some(resolved_height) = row.summary.resolved_height {
                                let blocks_since = chain_tip.index.saturating_sub(resolved_height);
                                blocks_since / 1440 // 1440 blocks per day
                            } else {
                                0
                            };
                        let unclaimed_status = if row.summary.resolved {
                            if days_since_draw >= 30 {
                                "Development Fund (30+ days)"
                            } else {
                                "Claimable (Winners have 30 days)"
                            }
                        } else {
                            "Not Yet Drawn"
                        };
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
                                        "gross_payout_koinu": winner.gross_payout_koinu,
                                        "tip_percent": winner.tip_percent,
                                        "tip_deduction_koinu": winner.tip_deduction_koinu,
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
                                    "cutoff_block": row.summary.cutoff_block,
                                    "tickets_close_eta_minutes": blocks_remaining,
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
                                                                        "days_since_draw": days_since_draw,
                                                                        "unclaimed_status": unclaimed_status,
                                    "rollover_occurred": row.summary.rollover_occurred,
                                    "current_ticket_count": row.summary.current_ticket_count,
                                    "payment_verification": if cmd.show_payment_verification {
                                        serde_json::json!({
                                            "mode": "strict_same_transaction",
                                            "status": "enforced",
                                            "verified_ticket_entries": row.summary.current_ticket_count,
                                            "rule": "same tx must pay exact ticket_price_koinu to deploy prize_pool_address"
                                        })
                                    } else {
                                        serde_json::Value::Null
                                    },
                                    "winners": winners,
                                })
                            );
                        } else {
                            println!("Lotto ID:               {}", row.summary.lotto_id);
                            println!("Inscription ID:         {}", row.summary.inscription_id);
                            println!("Deploy Height:          {}", row.summary.deploy_height);
                            println!("Draw Block:             {}", row.summary.draw_block);
                            println!(
                                "Tickets close at block {} (~{} minutes)",
                                row.summary.cutoff_block, blocks_remaining
                            );
                            println!("Ticket Price (koinu):   {}", row.summary.ticket_price_koinu);
                            println!("Prize Pool Address:     {}", row.summary.prize_pool_address);
                            println!("Fee Percent:            {}", row.summary.fee_percent);
                            println!("Resolution Mode:        {}", row.summary.resolution_mode);
                            println!("Rollover Enabled:       {}", row.summary.rollover_enabled);
                            println!(
                                "Guaranteed Min Prize:   {}",
                                row.summary
                                    .guaranteed_min_prize_koinu
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into())
                            );
                            println!(
                                "Current Ticket Count:   {}",
                                row.summary.current_ticket_count
                            );
                            println!("Resolved:               {}", row.summary.resolved);
                            println!(
                                "Resolved Height:        {}",
                                row.summary
                                    .resolved_height
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into())
                            );
                            println!(
                                "Verified Ticket Count:  {}",
                                row.summary
                                    .verified_ticket_count
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into())
                            );
                            println!(
                                "Verified Sales (koinu): {}",
                                row.summary
                                    .verified_sales_koinu
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into())
                            );
                            println!(
                                "Net Prize (koinu):      {}",
                                row.summary
                                    .net_prize_koinu
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "-".into())
                            );
                            if row.summary.resolved {
                                let prize_doge =
                                    row.summary.net_prize_koinu.unwrap_or(0) as f64 / 100_000_000.0;
                                println!(
                                    "Unclaimed Prize Pool:   {:.8} DOGE ({}, {} days since draw)",
                                    prize_doge, unclaimed_status, days_since_draw
                                );
                                if days_since_draw >= 30 {
                                    println!("  → Protocol developers thank participants for supporting development!");
                                } else {
                                    println!("  → Winners: claim within 30 days or prizes support protocol development");
                                }
                            }
                            println!("Rollover Occurred:      {}", row.summary.rollover_occurred);
                            if cmd.show_payment_verification {
                                println!(
                                    "Payment Verification:   strict_same_transaction (enforced)"
                                );
                                println!(
                                    "Verified Tickets:       {}",
                                    row.summary.current_ticket_count
                                );
                            }
                            if row.winners.is_empty() {
                                println!("Winners:                none");
                            } else {
                                println!("Winners:");
                                for winner in &row.winners {
                                    println!(
                                        "  rank {} ticket {} payout {} koinu (gross {} koinu, tip {}%, deduction {}) score {} inscription {}",
                                        winner.rank,
                                        winner.ticket_id,
                                        winner.payout_koinu,
                                        winner.gross_payout_koinu,
                                        winner.tip_percent,
                                        winner.tip_deduction_koinu,
                                        winner.score,
                                        winner.inscription_id
                                    );
                                }
                                if row.summary.resolved && days_since_draw < 30 {
                                    println!("  Winners must transfer their ticket inscription to claim prizes.");
                                    println!("  Unclaimed prizes after 30 days become protocol development funds.");
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
                    println!("DogeLotto Deployments (Total: {total})");
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
            LottoCommand::Burn(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;

                // Get ticket info from indexer
                let ticket_info =
                    doginals_indexer::lotto_get_ticket_info(&cmd.ticket_inscription_id, &config)
                        .await?;
                if ticket_info.is_none() {
                    if cmd.json {
                        println!("{{\"error\": \"Ticket not found\"}}");
                    } else {
                        println!("Error: Ticket {} not found", cmd.ticket_inscription_id);
                    }
                    process::exit(1);
                }

                let ticket_info = ticket_info.unwrap();
                let burn_address = &config.protocols.lotto.burn_address;

                // TODO: Build and broadcast a transfer transaction to burn_address
                // For now, just show info
                if cmd.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "ticket_inscription_id": cmd.ticket_inscription_id,
                            "lotto_id": ticket_info.lotto_id,
                            "ticket_id": ticket_info.ticket_id,
                            "burn_address": burn_address,
                            "action": "transfer_to_burn_address",
                            "reward": "1 Burn Point (10 points = 1 Burners Bonus Draw entry)"
                        })
                    );
                } else {
                    println!("Lotto Ticket Burn");
                    println!("─────────────────────────────────────────");
                    println!("Ticket Inscription ID:  {}", cmd.ticket_inscription_id);
                    println!("Lotto ID:               {}", ticket_info.lotto_id);
                    println!("Ticket ID:              {}", ticket_info.ticket_id);
                    println!("Burn Address:           {}", burn_address);
                    println!();
                    println!("To burn this ticket and earn 1 Burn Point:");
                    println!(
                        "  Send inscription {} to {}",
                        cmd.ticket_inscription_id, burn_address
                    );
                    println!("  (Use your Dogecoin wallet's inscription transfer feature)");
                    println!();
                    println!("Reward: +1 Burn Point");
                    println!("Every 10 Burn Points = 1 entry into monthly Burners Bonus Draw!");
                }
            }
            LottoCommand::Burners(cmd) => {
                let config = Config::from_file_path(&cmd.config_path)?;
                config.assert_doginals_config()?;

                if let Some(addr) = &cmd.address {
                    // Show burn points for specific address
                    let points = doginals_indexer::lotto_get_burn_points(addr, &config).await?;
                    if cmd.json {
                        if let Some(p) = points {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "owner_address": p.owner_address,
                                    "burn_points": p.burn_points,
                                    "bonus_draw_entries": p.burn_points / 10,
                                    "total_tickets_burned": p.total_tickets_burned,
                                    "last_burn_height": p.last_burn_height,
                                    "last_burn_timestamp": p.last_burn_timestamp,
                                })
                            );
                        } else {
                            println!("{{\"error\": \"No burn points found for address\"}}");
                        }
                    } else {
                        if let Some(p) = points {
                            println!("Burn Points for {}", p.owner_address);
                            println!("─────────────────────────────────────────");
                            println!("Burn Points:            {}", p.burn_points);
                            println!(
                                "Bonus Draw Entries:     {} (10 points per entry)",
                                p.burn_points / 10
                            );
                            println!("Total Tickets Burned:   {}", p.total_tickets_burned);
                            if let Some(h) = p.last_burn_height {
                                println!("Last Burn Block:        {}", h);
                            }
                        } else {
                            println!("No burn points found for address: {}", addr);
                        }
                    }
                } else {
                    // Show leaderboard
                    let burners =
                        doginals_indexer::lotto_get_top_burners(cmd.limit, &config).await?;
                    if cmd.json {
                        let json_rows: Vec<_> = burners
                            .iter()
                            .map(|b| {
                                serde_json::json!({
                                    "owner_address": b.owner_address,
                                    "burn_points": b.burn_points,
                                    "bonus_draw_entries": b.burn_points / 10,
                                    "total_tickets_burned": b.total_tickets_burned,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::json!({ "burners": json_rows }));
                    } else {
                        println!(
                            "Burners Leaderboard — Top {} (every 10 points = 1 Bonus Draw entry)",
                            cmd.limit
                        );
                        println!("{}", "─".repeat(90));
                        println!(
                            "{:<48} {:<12} {:<12} {}",
                            "Address", "Points", "Entries", "Burned"
                        );
                        println!("{}", "─".repeat(90));
                        for b in &burners {
                            println!(
                                "{:<48} {:<12} {:<12} {}",
                                b.owner_address,
                                b.burn_points,
                                b.burn_points / 10,
                                b.total_tickets_burned,
                            );
                        }
                    }
                }
            }
        },
        Protocol::Dogetag(subcmd) => {
            // Send only needs Dogecoin Core RPC — no DB pool required.
            if let DogetagCommand::Send(ref cmd) = subcmd {
                let config = Config::from_file_path(&cmd.config_path)?;
                let ctx = Context::empty();
                let amount_koinu = (cmd.amount * 100_000_000.0).round() as u64;
                if amount_koinu == 0 {
                    return Err("amount must be > 0 DOGE".into());
                }
                let result = broadcast_dogetag_send(&config, cmd, amount_koinu, &ctx)?;
                if cmd.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "txid": result.txid.to_string(),
                            "to": cmd.to,
                            "amount_koinu": result.amount_koinu,
                            "fee_koinu": result.fee_koinu,
                            "change_koinu": result.change_koinu,
                            "message": result.message,
                        })
                    );
                } else {
                    println!("Tagged send broadcast successfully!");
                    println!("  txid:    {}", result.txid);
                    println!("  to:      {}", cmd.to);
                    println!(
                        "  amount:  {} DOGE ({} koinu)",
                        cmd.amount, result.amount_koinu
                    );
                    println!("  message: \"{}\"", result.message);
                    println!("  fee:     {} koinu", result.fee_koinu);
                    if result.change_koinu > 0 {
                        println!("  change:  {} koinu", result.change_koinu);
                    }
                }
                return Ok(());
            }

            use doginals_indexer::db::doginals_pg;
            let config_path = match &subcmd {
                DogetagCommand::List(c) => &c.config_path,
                DogetagCommand::Search(c) => &c.config_path,
                DogetagCommand::Address(c) => &c.config_path,
                DogetagCommand::Send(_) => unreachable!(),
            };
            let config = Config::from_file_path(config_path)?;
            let doginals_config = config
                .doginals
                .as_ref()
                .ok_or("doginals database not configured")?;
            let pool = pg_pool(&doginals_config.db)?;
            let client = pool.get().await.map_err(|e| format!("pg pool: {e}"))?;

            match subcmd {
                DogetagCommand::List(cmd) => {
                    let tags = doginals_pg::list_dogetags(cmd.limit, cmd.offset, &client).await?;
                    if cmd.json {
                        let json: Vec<_> = tags
                            .iter()
                            .map(|t| {
                                serde_json::json!({
                                    "id": t.id,
                                    "txid": t.txid,
                                    "block_height": t.block_height,
                                    "block_timestamp": t.block_timestamp,
                                    "sender_address": t.sender_address,
                                    "message": t.message,
                                    "message_bytes": t.message_bytes,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&json).unwrap());
                    } else {
                        if tags.is_empty() {
                            println!("No dogetags indexed yet.");
                        } else {
                            println!(
                                "{:<12} {:<66} {:<44} {}",
                                "Block", "TxID", "Sender", "Message"
                            );
                            println!("{}", "-".repeat(120));
                            for t in &tags {
                                println!(
                                    "{:<12} {:<66} {:<44} {}",
                                    t.block_height,
                                    t.txid,
                                    t.sender_address.as_deref().unwrap_or("unknown"),
                                    &t.message[..t.message.len().min(60)],
                                );
                            }
                            println!("\n{} tag(s) shown.", tags.len());
                        }
                    }
                }
                DogetagCommand::Search(cmd) => {
                    let tags = doginals_pg::search_dogetags(&cmd.query, cmd.limit, &client).await?;
                    if cmd.json {
                        let json: Vec<_> = tags
                            .iter()
                            .map(|t| {
                                serde_json::json!({
                                    "id": t.id,
                                    "txid": t.txid,
                                    "block_height": t.block_height,
                                    "block_timestamp": t.block_timestamp,
                                    "sender_address": t.sender_address,
                                    "message": t.message,
                                    "message_bytes": t.message_bytes,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&json).unwrap());
                    } else {
                        if tags.is_empty() {
                            println!("No tags matching \"{}\".", cmd.query);
                        } else {
                            println!("{:<12} {:<44} {}", "Block", "Sender", "Message");
                            println!("{}", "-".repeat(100));
                            for t in &tags {
                                println!(
                                    "{:<12} {:<44} {}",
                                    t.block_height,
                                    t.sender_address.as_deref().unwrap_or("unknown"),
                                    t.message,
                                );
                            }
                            println!("\n{} tag(s) found.", tags.len());
                        }
                    }
                }
                DogetagCommand::Send(_) => unreachable!(),
                DogetagCommand::Address(cmd) => {
                    let tags =
                        doginals_pg::get_dogetags_by_address(&cmd.address, cmd.limit, &client)
                            .await?;
                    if cmd.json {
                        let json: Vec<_> = tags
                            .iter()
                            .map(|t| {
                                serde_json::json!({
                                    "id": t.id,
                                    "txid": t.txid,
                                    "block_height": t.block_height,
                                    "block_timestamp": t.block_timestamp,
                                    "sender_address": t.sender_address,
                                    "message": t.message,
                                    "message_bytes": t.message_bytes,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&json).unwrap());
                    } else {
                        if tags.is_empty() {
                            println!("No tags from address {}.", cmd.address);
                        } else {
                            println!("Tags from {}:", cmd.address);
                            println!("{}", "-".repeat(80));
                            for t in &tags {
                                println!("  Block #{}: {}", t.block_height, t.message);
                            }
                            println!("\n{} tag(s) found.", tags.len());
                        }
                    }
                }
            }
        }
        Protocol::Decode(cmd) => {
            let config = Config::from_file_path(&cmd.config_path)?;
            let txid_str = if let Some(iid) = &cmd.inscription_id {
                // Strip the 'i<N>' suffix: "abc123i0" -> "abc123"
                match iid.rfind('i') {
                    Some(pos) => iid[..pos].to_string(),
                    None => iid.clone(),
                }
            } else if let Some(t) = &cmd.txid {
                t.clone()
            } else {
                return Err("Provide --inscription-id or --txid".to_string());
            };

            let txid: Txid = txid_str
                .parse()
                .map_err(|e| format!("Invalid txid '{}': {}", txid_str, e))?;

            let ctx_rpc = Context::empty();
            let rpc = dogecoin::utils::bitcoind::dogecoin_get_client(&config.dogecoin, &ctx_rpc);
            let raw_hex = rpc
                .get_raw_transaction_hex(&txid, None)
                .map_err(|e| format!("getrawtransaction {}: {}", txid_str, e))?;
            let raw_bytes =
                hex::decode(&raw_hex).map_err(|e| format!("hex decode error: {}", e))?;
            let doge_tx: ::bitcoin::Transaction = ::bitcoin::consensus::deserialize(&raw_bytes)
                .map_err(|e| format!("tx deserialize error: {}", e))?;

            let envelopes =
                doginals::envelope::ParsedEnvelope::from_transactions_dogecoin(&[doge_tx]);

            if envelopes.is_empty() {
                if cmd.json {
                    println!("[]");
                } else {
                    println!("No inscriptions found in transaction {}", txid_str);
                }
                return Ok(());
            }

            if cmd.json {
                let out: Vec<_> = envelopes
                    .iter()
                    .enumerate()
                    .map(|(i, env)| {
                        let insc = &env.payload;
                        let content_type = insc
                            .content_type
                            .as_ref()
                            .and_then(|ct| std::str::from_utf8(ct).ok())
                            .map(str::to_string);
                        let metaprotocol = insc
                            .metaprotocol
                            .as_ref()
                            .and_then(|mp| std::str::from_utf8(mp).ok())
                            .map(str::to_string);
                        let body_text = insc.body.as_ref().and_then(|b| {
                            let ct = content_type.as_deref().unwrap_or("");
                            if ct.starts_with("text/") || ct == "application/json" {
                                std::str::from_utf8(b).ok().map(str::to_string)
                            } else {
                                None
                            }
                        });
                        let body_hex = if cmd.hex {
                            insc.body.as_ref().map(hex::encode)
                        } else {
                            None
                        };
                        serde_json::json!({
                            "inscription_id": format!("{}i{}", txid_str, i),
                            "content_type": content_type,
                            "content_length": insc.body.as_ref().map(|b| b.len()),
                            "metaprotocol": metaprotocol,
                            "body_text": body_text,
                            "body_hex": body_hex,
                            "input": env.input,
                            "offset": env.offset,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            } else {
                for (i, env) in envelopes.iter().enumerate() {
                    let insc = &env.payload;
                    let content_type = insc
                        .content_type
                        .as_ref()
                        .and_then(|ct| std::str::from_utf8(ct).ok())
                        .unwrap_or("unknown");
                    let body_len = insc.body.as_ref().map(|b| b.len()).unwrap_or(0);
                    println!("Inscription #{i}:");
                    println!("  inscription_id:  {}i{}", txid_str, i);
                    println!("  content_type:    {content_type}");
                    println!("  content_length:  {body_len} bytes");
                    if let Some(mp) = insc
                        .metaprotocol
                        .as_ref()
                        .and_then(|mp| std::str::from_utf8(mp).ok())
                    {
                        println!("  metaprotocol:    {mp}");
                    }
                    if let Some(body) = &insc.body {
                        let ct = content_type;
                        if ct.starts_with("text/") || ct == "application/json" {
                            if let Ok(s) = std::str::from_utf8(body) {
                                let preview: String = s.chars().take(300).collect();
                                println!("  body_preview:    {preview}");
                            }
                        }
                        if cmd.hex {
                            println!("  body_hex:        {}", hex::encode(body));
                        }
                    }
                }
            }
        }
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

struct LottoNumberDefaults {
    template: &'static str,
    main_pick: u16,
    main_max: u16,
    bonus_pick: u16,
    bonus_max: u16,
}

fn lotto_number_defaults(lotto_id: &str) -> LottoNumberDefaults {
    match lotto_id {
        "doge-4-20-blaze" => LottoNumberDefaults {
            template: "closest_wins",
            main_pick: 4,
            main_max: 20,
            bonus_pick: 3,
            bonus_max: 20,
        },
        "deno" => LottoNumberDefaults {
            template: "closest_wins",
            main_pick: 10,
            main_max: 80,
            bonus_pick: 0,
            bonus_max: 0,
        },
        _ => LottoNumberDefaults {
            template: "closest_wins",
            main_pick: 69,
            main_max: 420,
            bonus_pick: 0,
            bonus_max: 0,
        },
    }
}

fn normalize_template(value: &str) -> Result<&'static str, String> {
    match value {
        "closest_wins" => Ok("closest_wins"),
        "powerball_dual_drum" => Ok("powerball_dual_drum"),
        "6_49_classic" => Ok("6_49_classic"),
        "rollover_jackpot" => Ok("rollover_jackpot"),
        "always_winner" => Ok("always_winner"),
        "life_annuity" => Ok("life_annuity"),
        "custom" => Ok("custom"),
        _ => Err(format!(
            "invalid template: {} (expected closest_wins, powerball_dual_drum, 6_49_classic, rollover_jackpot, always_winner, life_annuity, or custom)",
            value
        )),
    }
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
    tip_koinu: u64,
    fee_koinu: u64,
    change_koinu: u64,
}

struct DogetagSendResult {
    txid: Txid,
    amount_koinu: u64,
    fee_koinu: u64,
    change_koinu: u64,
    message: String,
}

fn calc_tag_send_fee(msg_len: usize) -> u64 {
    // P2PKH input (148) + payment output (34) + OP_RETURN output (8+1+1+1+msg_len)
    // + change output (34) + overhead (10)
    let total = 148 + 34 + 11 + msg_len + 34 + 10;
    (total as f64 * DEFAULT_LOTTO_FEE_RATE).ceil() as u64
}

fn broadcast_dogetag_send(
    config: &Config,
    cmd: &DogetagSendCommand,
    amount_koinu: u64,
    ctx: &Context,
) -> Result<DogetagSendResult, String> {
    let msg_bytes = cmd.message.as_bytes();
    if msg_bytes.len() > 80 {
        return Err(format!(
            "message is {} bytes, max is 80 UTF-8 bytes",
            msg_bytes.len()
        ));
    }
    if msg_bytes.is_empty() {
        return Err("message cannot be empty".into());
    }

    let client = dogecoin::utils::bitcoind::dogecoin_get_client(&config.dogecoin, ctx);
    let recipient_script = parse_dogecoin_address(&cmd.to)?;

    let push_bytes: &bitcoin::script::PushBytes = msg_bytes.try_into().map_err(|_| {
        format!(
            "message too long to encode as OP_RETURN push ({} bytes)",
            msg_bytes.len()
        )
    })?;
    let op_return_script = ScriptBuf::builder()
        .push_opcode(bitcoin::opcodes::all::OP_RETURN)
        .push_slice(push_bytes)
        .into_script();

    let fee_koinu = calc_tag_send_fee(msg_bytes.len());
    let required_koinu = amount_koinu.saturating_add(fee_koinu);

    let (funding_txid, funding_vout, funding_value, funding_script) =
        select_lotto_utxo(&client, required_koinu, false)
            .map_err(|e| e.replace("atomic lotto mint", "tagged send"))?;

    let funding_koinu = funding_value.to_sat();
    let change_koinu = funding_koinu
        .saturating_sub(amount_koinu)
        .saturating_sub(fee_koinu);

    let mut outputs = vec![
        TxOut {
            value: Amount::from_sat(amount_koinu),
            script_pubkey: recipient_script,
        },
        TxOut {
            value: Amount::ZERO,
            script_pubkey: op_return_script,
        },
    ];

    if change_koinu > 0 {
        let change_address: String = client
            .call("getrawchangeaddress", &[])
            .map_err(|e| format!("unable to get raw change address: {e}"))?;
        outputs.push(TxOut {
            value: Amount::from_sat(change_koinu),
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
            script_sig: ScriptBuf::from(build_script_sig(&[], &sig_bytes, &pubkey_bytes)),
            sequence: Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }],
        output: outputs,
    };

    let txid = client
        .send_raw_transaction(&final_tx)
        .map_err(|e| format!("unable to broadcast tagged send transaction: {e}"))?;

    Ok(DogetagSendResult {
        txid,
        amount_koinu,
        fee_koinu,
        change_koinu,
        message: cmd.message.clone(),
    })
}

fn broadcast_atomic_lotto_mint(
    config: &Config,
    prize_pool_address: &str,
    ticket_price_koinu: u64,
    tip_percent: u8,
    payload: &str,
    ctx: &Context,
) -> Result<AtomicLottoMintResult, String> {
    let client = dogecoin::utils::bitcoind::dogecoin_get_client(&config.dogecoin, ctx);
    let script_segments = build_lotto_inscription_segments(payload.as_bytes());
    // extra_amount = ticket_price_koinu * (tip_percent / 100)
    let extra_amount_koinu = ticket_price_koinu.saturating_mul(tip_percent as u64) / 100;
    if tip_percent > 0 && extra_amount_koinu == 0 {
        return Err(
            "tip_percent is non-zero but computed extra amount is 0 koinu; increase ticket price or reduce rounding loss"
                .into(),
        );
    }
    let output_count =
        1 + usize::from(ticket_price_koinu > 0) + usize::from(extra_amount_koinu > 0);
    let fee_koinu = calc_lotto_fee(
        script_sig_size(&script_segments),
        output_count,
        DEFAULT_LOTTO_FEE_RATE,
    );
    let required_koinu = ticket_price_koinu
        .saturating_add(extra_amount_koinu)
        .saturating_add(fee_koinu);
    let (funding_txid, funding_vout, funding_value, funding_script) = select_lotto_utxo(
        &client,
        required_koinu,
        ticket_price_koinu == 0 && extra_amount_koinu == 0,
    )?;

    let funding_koinu = funding_value.to_sat();
    let prize_pool_script = parse_dogecoin_address(prize_pool_address)?;
    let protocol_dev_script = if extra_amount_koinu > 0 {
        Some(parse_dogecoin_address(
            config.protocols.lotto.protocol_dev_address.trim(),
        )?)
    } else {
        None
    };
    let (outputs, change_koinu) = build_atomic_lotto_outputs(
        &client,
        &prize_pool_script,
        protocol_dev_script.as_ref(),
        ticket_price_koinu,
        extra_amount_koinu,
        funding_koinu,
        fee_koinu,
    )?;

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

    let final_tx = build_atomic_lotto_signed_tx(
        funding_txid,
        funding_vout,
        outputs,
        &script_segments,
        &sig_bytes,
        &pubkey_bytes,
    );

    let txid = client
        .send_raw_transaction(&final_tx)
        .map_err(|e| format!("unable to broadcast atomic lotto mint transaction: {e}"))?;

    Ok(AtomicLottoMintResult {
        txid,
        tip_koinu: extra_amount_koinu,
        fee_koinu,
        change_koinu,
    })
}

fn build_atomic_lotto_outputs(
    client: &dogecoin::bitcoincore_rpc::Client,
    prize_pool_script: &ScriptBuf,
    protocol_dev_script: Option<&ScriptBuf>,
    ticket_price_koinu: u64,
    extra_amount_koinu: u64,
    funding_koinu: u64,
    fee_koinu: u64,
) -> Result<(Vec<TxOut>, u64), String> {
    let required_koinu = ticket_price_koinu
        .saturating_add(extra_amount_koinu)
        .saturating_add(fee_koinu);
    let change_koinu = funding_koinu.saturating_sub(required_koinu);

    if ticket_price_koinu == 0 && extra_amount_koinu == 0 && change_koinu == 0 {
        return Err(
            "free lotto mint requires a wallet UTXO larger than the estimated fee so the transaction can keep one standard output"
                .into(),
        );
    }

    let mut outputs = Vec::new();

    // Atomic payment output in the same transaction as the inscription envelope.
    if ticket_price_koinu > 0 {
        outputs.push(TxOut {
            value: Amount::from_sat(ticket_price_koinu),
            script_pubkey: prize_pool_script.clone(),
        });
    }

    // Optional immutable protocol-dev tip output in the same atomic mint tx.
    if extra_amount_koinu > 0 {
        let Some(protocol_dev_script) = protocol_dev_script else {
            return Err("protocol dev address is required when tip amount is non-zero".into());
        };
        outputs.push(TxOut {
            value: Amount::from_sat(extra_amount_koinu),
            script_pubkey: protocol_dev_script.clone(),
        });
    }

    let effective_change_koinu = if change_koinu > 0 || ticket_price_koinu == 0 {
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
        change_value
    } else {
        0
    };

    Ok((outputs, effective_change_koinu))
}

fn build_atomic_lotto_signed_tx(
    funding_txid: Txid,
    funding_vout: u32,
    outputs: Vec<TxOut>,
    script_segments: &[Vec<u8>],
    sig_bytes: &[u8],
    pubkey_bytes: &[u8],
) -> Transaction {
    Transaction {
        version: bitcoin::transaction::Version(1),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: funding_txid,
                vout: funding_vout,
            },
            script_sig: ScriptBuf::from(build_script_sig(script_segments, sig_bytes, pubkey_bytes)),
            sequence: Sequence::MAX,
            witness: bitcoin::Witness::new(),
        }],
        output: outputs,
    }
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
        Some(Ok(bitcoin::script::Instruction::PushBytes(push_bytes))) => {
            push_bytes.as_bytes().to_vec()
        }
        other => {
            return Err(format!(
                "unexpected signature instruction in signed lotto mint input: {:?}",
                other
            ))
        }
    };

    let pubkey = match instructions.next() {
        Some(Ok(bitcoin::script::Instruction::PushBytes(push_bytes))) => {
            push_bytes.as_bytes().to_vec()
        }
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
        return Err(format!(
            "invalid Dogecoin address '{}': empty payload",
            addr
        ));
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
        _ => Err(format!(
            "unsupported Dogecoin address version '{}' for '{}'",
            version, addr
        )),
    }
}
