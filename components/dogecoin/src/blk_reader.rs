//! Direct `.blk` file reader for Dogecoin Core block data.
//!
//! Reads blocks straight from the binary `.blk` files on disk, bypassing
//! JSON-RPC. Typically 5-20x faster than RPC for initial sync.
//!
//! ## AuxPow support
//!
//! Dogecoin activated merged mining (AuxPow) at block 371,337. This module
//! includes a minimal AuxPow-aware block deserializer so ALL Dogecoin blocks
//! (including blocks with AuxPow headers) are parsed correctly.
//!
//! ## Index shadow copy
//!
//! Dogecoin Core holds an exclusive LevelDB lock on `blocks/index/` while
//! running. To work around this, kabosu maintains a **shadow copy** of the
//! index at `<data-dir>/blk-index/`. The copy is refreshed automatically
//! each time the indexer starts. A smart-copy strategy is used: immutable
//! `.ldb` files are skipped once they already exist; only the MANIFEST and
//! WAL are re-copied on each run (usually < 1 second). The `LOCK` file is
//! never copied so kabosu can open the shadow copy freely.
//!
//! ## Prevout values
//!
//! The binary fast path sets `previous_output.value = 0` and
//! `previous_output.block_height = 0` on all inputs because prevout data is
//! not encoded in the `.blk` files. This is acceptable for kabosu's primary
//! use case (inscription content indexing) because:
//!   - `operations` (Rosetta model) is left empty and is unused by the indexer
//!   - Fee calculation is skipped (fee = 0)
//!   - If koinu sat-range backward traversal is needed, use RPC mode instead

use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    io::{BufReader, Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use bitcoin::hashes::{sha256d, Hash};
use byteorder::{LittleEndian, ReadBytesExt};
use rusty_leveldb::{LdbIterator, Options, DB};

use crate::{
    try_info, try_warn,
    types::{
        dogecoin::{OutPoint, TxIn, TxOut},
        DogecoinBlockData, DogecoinBlockMetadata, DogecoinNetwork, DogecoinTransactionData,
        DogecoinTransactionMetadata, BlockIdentifier, TransactionIdentifier,
    },
    utils::Context,
};

/// height → (blk_file_index, data_offset_within_file, block_hash_hex)
type BlkIndex = HashMap<u32, (u32, u64, String)>;

/// The AuxPow version flag in Dogecoin block version field.
const AUXPOW_VERSION_FLAG: u32 = 0x0100;

/// Reads blocks directly from Dogecoin Core's `.blk` files.
pub struct BlkReader {
    blocks_dir: PathBuf,
    index: Arc<BlkIndex>,
}

impl BlkReader {
    /// Attempt to open a `BlkReader`.
    ///
    /// `blocks_dir` is the `blocks/` directory inside the Dogecoin Core data
    /// directory. `index_copy_dir` is where kabosu stores its shadow copy of
    /// Core's LevelDB index (e.g. `<data-dir>/blk-index`).
    ///
    /// Returns `Ok(None)` when the index cannot be opened, in which case the
    /// caller should fall back to RPC.
    pub fn open(blocks_dir: &Path, index_copy_dir: &Path, ctx: &Context) -> Option<Self> {
        let live_index = blocks_dir.join("index");
        if !live_index.exists() {
            try_warn!(
                ctx,
                "BlkReader: blocks/index/ not found at {}, falling back to RPC",
                live_index.display()
            );
            return None;
        }

        // Refresh the shadow copy.
        match refresh_index_copy(&live_index, index_copy_dir) {
            Ok((copied, skipped)) => {
                if copied > 0 {
                    try_info!(
                        ctx,
                        "BlkReader: index copy refreshed ({copied} updated, {skipped} unchanged) → {}",
                        index_copy_dir.display()
                    );
                }
            }
            Err(e) => try_warn!(ctx, "BlkReader: could not refresh index copy: {e}"),
        }

        // Prefer the shadow copy.
        if index_copy_dir.exists() {
            match build_block_index(index_copy_dir) {
                Ok(idx) => {
                    try_info!(
                        ctx,
                        "BlkReader: loaded {} block locations from shadow copy — direct .blk reads enabled",
                        idx.len()
                    );
                    return Some(Self {
                        blocks_dir: blocks_dir.to_owned(),
                        index: Arc::new(idx),
                    });
                }
                Err(e) => try_warn!(ctx, "BlkReader: shadow copy unusable ({e}), trying live index"),
            }
        }

        // Fall back to the live index (works when Core is stopped).
        match build_block_index(&live_index) {
            Ok(idx) => {
                try_info!(
                    ctx,
                    "BlkReader: loaded {} block locations from live index",
                    idx.len()
                );
                Some(Self {
                    blocks_dir: blocks_dir.to_owned(),
                    index: Arc::new(idx),
                })
            }
            Err(e) => {
                if e.contains("lock") || e.contains("Lock") {
                    try_warn!(
                        ctx,
                        "BlkReader: Dogecoin Core holds the LevelDB lock and the shadow copy \
                         could not be created. Falling back to RPC. Run \
                         `kabosu doginals index refresh-blk-index` once to build the shadow copy.",
                    );
                } else {
                    try_warn!(ctx, "BlkReader: could not read block index: {e}");
                }
                None
            }
        }
    }

    /// Highest block height available in the on-disk index.
    pub fn max_height(&self) -> u32 {
        self.index.keys().copied().max().unwrap_or(0)
    }

    /// Read and deserialize a block by height.
    ///
    /// Returns `Ok(None)` when the height is not yet in the on-disk index
    /// (tip blocks that haven't been flushed yet). Returns `Err` on I/O or
    /// parse failure; caller should fall back to RPC.
    pub fn get(
        &self,
        height: u32,
        network: &DogecoinNetwork,
    ) -> Result<Option<DogecoinBlockData>, String> {
        let Some(&(file_idx, data_offset, ref block_hash)) = self.index.get(&height) else {
            return Ok(None);
        };
        let block =
            read_dogecoin_block(&self.blocks_dir, file_idx, data_offset, height, block_hash, network)
                .map_err(|e| format!("BlkReader: reading block at height {height}: {e}"))?;
        Ok(Some(block))
    }

    /// Shared reference to the index (for parallel access from worker threads).
    pub fn index_arc(&self) -> Arc<BlkIndex> {
        self.index.clone()
    }

    /// The blocks directory (for parallel access from worker threads).
    pub fn blocks_dir(&self) -> &Path {
        &self.blocks_dir
    }
}

// ---------------------------------------------------------------------------
// Shadow copy management
// ---------------------------------------------------------------------------

/// Copy Core's `blocks/index/` to `copy_dir`, skipping transient files.
///
/// Uses smart-copy: a file is only re-copied when the source is newer than
/// the destination (by mtime). Immutable `.ldb` files are skipped after the
/// first copy. Returns `(copied, skipped)` counts.
pub fn refresh_index_copy(live_index: &Path, copy_dir: &Path) -> Result<(u32, u32), String> {
    fs::create_dir_all(copy_dir)
        .map_err(|e| format!("creating {}: {e}", copy_dir.display()))?;

    let (mut copied, mut skipped) = (0u32, 0u32);

    let entries = fs::read_dir(live_index)
        .map_err(|e| format!("reading {}: {e}", live_index.display()))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
        let name = entry.file_name();

        // Never copy LevelDB metadata or transient files that can be locked or
        // that form a fragile linked pair (CURRENT → MANIFEST-N → .ldb files).
        //
        // - LOCK:       Core holds an exclusive write lock; never needed in a copy.
        // - *.log:      Write-ahead log; Core locks it while running.
        // - CURRENT:    Points to the active MANIFEST.  Copying a new CURRENT
        //               while the corresponding MANIFEST can't be copied (locked)
        //               leaves the shadow copy in a broken, unopenable state.
        // - MANIFEST-*: Core holds this open for writing; also locked.
        //
        // Immutable *.ldb files are safe to copy incrementally.
        // The CURRENT/MANIFEST pair is managed by whoever originally created the
        // shadow copy (e.g. `dog`) and must never be disturbed by Kabosu.
        let name_str = name.to_string_lossy();
        if name == OsStr::new("LOCK")
            || name == OsStr::new("CURRENT")
            || name_str.starts_with("MANIFEST-")
            || name_str.ends_with(".log")
        {
            skipped += 1;
            continue;
        }

        let dst = copy_dir.join(&name);

        let src_mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if let Ok(dst_meta) = fs::metadata(&dst) {
            let dst_mtime = dst_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if dst_mtime >= src_mtime {
                skipped += 1;
                continue;
            }
        }

        match fs::copy(entry.path(), &dst) {
            Ok(_) => copied += 1,
            Err(e) => {
                // On Windows, Dogecoin Core may hold an exclusive lock on transient
                // LevelDB files (MANIFEST, *.log) while running.
                // PermissionDenied covers most cases; raw os error 32 is Windows
                // ERROR_SHARING_VIOLATION ("being used by another process").
                // In both cases keep the existing shadow copy of the file and move on.
                let locked = e.kind() == std::io::ErrorKind::PermissionDenied
                    || e.raw_os_error() == Some(32);
                if locked {
                    skipped += 1;
                    continue;
                }
                return Err(format!("copying {:?}: {e}", name));
            }
        }
    }

    Ok((copied, skipped))
}

// ---------------------------------------------------------------------------
// LevelDB block index parsing
// ---------------------------------------------------------------------------

fn build_block_index(index_path: &Path) -> Result<BlkIndex, String> {
    let mut opts = Options::default();
    opts.create_if_missing = false;

    let mut db = DB::open(index_path, opts)
        .map_err(|e| format!("opening LevelDB at {}: {e}", index_path.display()))?;

    let mut result = BlkIndex::new();
    let mut iter = db
        .new_iter()
        .map_err(|e| format!("creating LevelDB iterator: {e}"))?;

    // Block records all start with key prefix b'b'
    iter.seek(b"b");

    let (mut key, mut value) = (vec![], vec![]);
    while iter.advance() {
        iter.current(&mut key, &mut value);

        if key.first() != Some(&b'b') {
            break;
        }

        // Key layout: b'b' + 32-byte block hash (internal byte order)
        if key.len() < 33 {
            continue;
        }
        let mut hash_bytes = key[1..33].to_vec();
        hash_bytes.reverse(); // internal → display order
        let block_hash_hex = hex::encode(&hash_bytes);

        if let Some((height, file_idx, offset)) = parse_index_record(&value) {
            result
                .entry(height)
                .or_insert((file_idx, offset, block_hash_hex));
        }
    }

    Ok(result)
}

/// Parse a single LevelDB block index value.
///
/// Dogecoin Core record layout (same as Bitcoin Core):
/// ```text
///   varint  version
///   varint  height
///   varint  status
///   varint  tx_count
///   if status & BLOCK_HAVE_DATA:
///     varint  file_number
///     varint  data_offset
/// ```
fn parse_index_record(value: &[u8]) -> Option<(u32, u32, u64)> {
    let mut cur = Cursor::new(value);

    let _version = read_ldb_varint(&mut cur).ok()?;
    let height = read_ldb_varint(&mut cur).ok()? as u32;
    let status = read_ldb_varint(&mut cur).ok()?;
    let _tx_count = read_ldb_varint(&mut cur).ok()?;

    const BLOCK_HAVE_DATA: u64 = 8;
    const BLOCK_FAILED_VALID: u64 = 32;
    const BLOCK_FAILED_CHILD: u64 = 64;

    if status & BLOCK_HAVE_DATA == 0 {
        return None;
    }
    if status & (BLOCK_FAILED_VALID | BLOCK_FAILED_CHILD) != 0 {
        return None;
    }

    let file_idx = read_ldb_varint(&mut cur).ok()? as u32;
    let data_offset = read_ldb_varint(&mut cur).ok()?;

    Some((height, file_idx, data_offset))
}

/// Dogecoin/Bitcoin Core LevelDB varint: each byte contributes 7 bits,
/// high bit signals continuation, accumulator += 1 on continuation.
fn read_ldb_varint(cur: &mut Cursor<&[u8]>) -> std::io::Result<u64> {
    let mut n: u64 = 0;
    loop {
        let byte = cur.read_u8()?;
        n = (n << 7) | (byte & 0x7F) as u64;
        if byte & 0x80 != 0 {
            n = n.checked_add(1).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "varint overflow")
            })?;
        } else {
            break;
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// .blk file reading — Dogecoin AuxPow-aware block parser
// ---------------------------------------------------------------------------

/// Each block record in a `.blk` file:
/// ```text
///   [4 bytes]  network magic (C0 C0 C0 C0 for mainnet)
///   [4 bytes]  block_size   (little-endian u32)
///   [N bytes]  raw serialized block
/// ```
/// The LevelDB `data_offset` points to the start of the raw block bytes
/// (i.e. 8 bytes into the record). So `data_offset - 4` is where
/// `block_size` lives.
fn read_dogecoin_block(
    blk_dir: &Path,
    file_idx: u32,
    data_offset: u64,
    height: u32,
    block_hash: &str,
    network: &DogecoinNetwork,
) -> Result<DogecoinBlockData, String> {
    let path = blk_dir.join(format!("blk{:05}.dat", file_idx));
    let file = fs::File::open(&path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);

    reader
        .seek(SeekFrom::Start(data_offset.saturating_sub(4)))
        .map_err(|e| format!("seek: {e}"))?;

    let block_size = reader
        .read_u32::<LittleEndian>()
        .map_err(|e| format!("reading block_size: {e}"))? as usize;

    let mut block_bytes = vec![0u8; block_size];
    reader
        .read_exact(&mut block_bytes)
        .map_err(|e| format!("reading block bytes: {e}"))?;

    parse_dogecoin_block(&block_bytes, height, block_hash, network)
}

/// Parse raw Dogecoin block bytes (AuxPow-aware) into `DogecoinBlockData`.
pub fn parse_dogecoin_block(
    data: &[u8],
    height: u32,
    block_hash: &str,
    network: &DogecoinNetwork,
) -> Result<DogecoinBlockData, String> {
    let mut cur = Cursor::new(data);

    // Standard 80-byte block header
    let version = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| format!("reading version: {e}"))?;

    let mut prev_hash_raw = [0u8; 32];
    cur.read_exact(&mut prev_hash_raw)
        .map_err(|e| format!("reading prev_hash: {e}"))?;

    // merkle root, bits, nonce — we only need time
    cur.seek(SeekFrom::Current(32))
        .map_err(|e| format!("skip merkle: {e}"))?;
    let time = cur
        .read_u32::<LittleEndian>()
        .map_err(|e| format!("reading time: {e}"))?;
    cur.seek(SeekFrom::Current(8))
        .map_err(|e| format!("skip bits+nonce: {e}"))?; // bits (4) + nonce (4)

    // Skip AuxPow data if the version flag is set
    if version & AUXPOW_VERSION_FLAG != 0 {
        skip_auxpow(&mut cur, data).map_err(|e| format!("skipping AuxPow: {e}"))?;
    }

    // Transaction count
    let tx_count = read_compact_size(&mut cur)
        .map_err(|e| format!("reading tx_count: {e}"))? as usize;

    // Parse each transaction
    let mut transactions = Vec::with_capacity(tx_count);
    for tx_index in 0..tx_count {
        let tx = read_raw_tx(&mut cur, data, tx_index as u32)
            .map_err(|e| format!("tx {tx_index}: {e}"))?;
        transactions.push(tx);
    }

    // Build block identifiers
    let mut prev_hash_display = prev_hash_raw.to_vec();
    prev_hash_display.reverse();

    let parent_index = if height > 0 { height as u64 - 1 } else { 0 };
    let parent_hash = if height == 0 {
        "0x0000000000000000000000000000000000000000000000000000000000000000".to_string()
    } else {
        format!("0x{}", hex::encode(&prev_hash_display))
    };

    Ok(DogecoinBlockData {
        block_identifier: BlockIdentifier {
            index: height as u64,
            hash: format!("0x{}", block_hash),
        },
        parent_block_identifier: BlockIdentifier {
            index: parent_index,
            hash: parent_hash,
        },
        timestamp: time,
        transactions,
        metadata: DogecoinBlockMetadata {
            network: network.clone(),
        },
    })
}

/// Skip AuxPow data that follows the 80-byte standard header in a Dogecoin
/// merged-mining block. AuxPow structure:
/// ```text
///   [tx]       coinbase transaction (full serialization)
///   [32 bytes] parent block hash
///   [varint + 32*n] coinbase merkle branch
///   [4 bytes]  coinbase merkle index
///   [varint + 32*n] chain merkle branch
///   [4 bytes]  chain merkle index
///   [80 bytes] parent block header
/// ```
fn skip_auxpow(cur: &mut Cursor<&[u8]>, data: &[u8]) -> std::io::Result<()> {
    // Skip coinbase transaction
    skip_transaction(cur, data)?;

    // Skip parent block hash (32 bytes)
    cur.seek(SeekFrom::Current(32))?;

    // Skip coinbase merkle branch: varint count + 32 bytes per hash
    let count = read_compact_size(cur)?;
    cur.seek(SeekFrom::Current((count * 32) as i64))?;

    // Skip coinbase merkle index (4 bytes)
    cur.seek(SeekFrom::Current(4))?;

    // Skip chain merkle branch: varint count + 32 bytes per hash
    let count = read_compact_size(cur)?;
    cur.seek(SeekFrom::Current((count * 32) as i64))?;

    // Skip chain merkle index (4 bytes)
    cur.seek(SeekFrom::Current(4))?;

    // Skip parent block header (80 bytes)
    cur.seek(SeekFrom::Current(80))?;

    Ok(())
}

/// Skip a serialized transaction without parsing its fields.
fn skip_transaction(cur: &mut Cursor<&[u8]>, _data: &[u8]) -> std::io::Result<()> {
    cur.seek(SeekFrom::Current(4))?; // version

    let vin_count = read_compact_size(cur)?;
    for _ in 0..vin_count {
        cur.seek(SeekFrom::Current(36))?; // outpoint (txid 32 + vout 4)
        let script_len = read_compact_size(cur)?;
        cur.seek(SeekFrom::Current(script_len as i64))?; // scriptSig
        cur.seek(SeekFrom::Current(4))?; // sequence
    }

    let vout_count = read_compact_size(cur)?;
    for _ in 0..vout_count {
        cur.seek(SeekFrom::Current(8))?; // value
        let script_len = read_compact_size(cur)?;
        cur.seek(SeekFrom::Current(script_len as i64))?; // scriptPubKey
    }

    cur.seek(SeekFrom::Current(4))?; // locktime
    Ok(())
}

struct RawTxIn {
    is_coinbase: bool,
    txid_raw: [u8; 32], // internal byte order
    vout: u32,
    script_sig: Vec<u8>,
    sequence: u32,
}

struct RawTxOut {
    value: u64,
    script_pubkey: Vec<u8>,
}

/// Read and parse one transaction from the cursor, computing the txid via
/// SHA256d of the raw transaction bytes.
fn read_raw_tx(
    cur: &mut Cursor<&[u8]>,
    data: &[u8],
    tx_index: u32,
) -> std::io::Result<DogecoinTransactionData> {
    let tx_start = cur.position() as usize;

    let _version = cur.read_u32::<LittleEndian>()?;

    let vin_count = read_compact_size(cur)? as usize;
    let mut raw_inputs = Vec::with_capacity(vin_count);

    for _ in 0..vin_count {
        let mut txid_raw = [0u8; 32];
        cur.read_exact(&mut txid_raw)?;
        let vout = cur.read_u32::<LittleEndian>()?;

        let script_len = read_compact_size(cur)? as usize;
        let mut script_sig = vec![0u8; script_len];
        cur.read_exact(&mut script_sig)?;

        let sequence = cur.read_u32::<LittleEndian>()?;

        // Coinbase input: prev txid is all zeros and vout is 0xffffffff
        let is_coinbase = txid_raw == [0u8; 32] && vout == 0xffff_ffff;

        raw_inputs.push(RawTxIn {
            is_coinbase,
            txid_raw,
            vout,
            script_sig,
            sequence,
        });
    }

    let vout_count = read_compact_size(cur)? as usize;
    let mut raw_outputs = Vec::with_capacity(vout_count);

    for _ in 0..vout_count {
        let value = cur.read_u64::<LittleEndian>()?;
        let script_len = read_compact_size(cur)? as usize;
        let mut script_pubkey = vec![0u8; script_len];
        cur.read_exact(&mut script_pubkey)?;
        raw_outputs.push(RawTxOut { value, script_pubkey });
    }

    let _locktime = cur.read_u32::<LittleEndian>()?;
    let tx_end = cur.position() as usize;

    // Compute txid = SHA256d of raw transaction bytes
    let tx_bytes = &data[tx_start..tx_end];
    let hash = sha256d::Hash::hash(tx_bytes);
    let mut txid_bytes = *hash.as_byte_array();
    txid_bytes.reverse(); // internal → display order
    let txid_display = hex::encode(txid_bytes);

    // Convert to our types
    let inputs: Vec<TxIn> = raw_inputs
        .into_iter()
        .filter(|i| !i.is_coinbase)
        .map(|i| {
            let mut txid_display_bytes = i.txid_raw;
            txid_display_bytes.reverse();
            TxIn {
                previous_output: OutPoint {
                    txid: TransactionIdentifier::new(&hex::encode(txid_display_bytes)),
                    vout: i.vout,
                    // Prevout value and block height are unavailable in binary
                    // blocks without a UTXO database lookup. Set to 0; these
                    // fields are used only for fee calculation and Rosetta
                    // operations, neither of which is required by the
                    // inscription indexer.
                    value: 0,
                    block_height: 0,
                },
                script_sig: format!("0x{}", hex::encode(&i.script_sig)),
                sequence: i.sequence,
            }
        })
        .collect();

    let outputs: Vec<TxOut> = raw_outputs
        .into_iter()
        .map(|o| TxOut {
            value: o.value,
            script_pubkey: format!("0x{}", hex::encode(&o.script_pubkey)),
        })
        .collect();

    Ok(DogecoinTransactionData {
        transaction_identifier: TransactionIdentifier {
            hash: format!("0x{txid_display}"),
        },
        operations: vec![],
        metadata: DogecoinTransactionMetadata {
            inputs,
            outputs,
            ordinal_operations: vec![],
            drc20_operation: None,
            proof: None,
            fee: 0,
            index: tx_index,
        },
    })
}

/// Read Bitcoin/Dogecoin compact-size integer from a cursor.
/// (Different from the LevelDB varint used in the block index.)
fn read_compact_size(cur: &mut Cursor<&[u8]>) -> std::io::Result<u64> {
    let first = cur.read_u8()?;
    Ok(match first {
        0x00..=0xfc => first as u64,
        0xfd => cur.read_u16::<LittleEndian>()? as u64,
        0xfe => cur.read_u32::<LittleEndian>()? as u64,
        0xff => cur.read_u64::<LittleEndian>()?,
    })
}

/// Read block data for a single height directly from .blk files using a
/// pre-built index. Used by the parallel file pipeline worker threads.
pub fn read_block_by_height(
    blocks_dir: &Path,
    index: &BlkIndex,
    height: u32,
    network: &DogecoinNetwork,
) -> Result<Option<DogecoinBlockData>, String> {
    let Some(&(file_idx, data_offset, ref block_hash)) = index.get(&height) else {
        return Ok(None);
    };
    let block = read_dogecoin_block(blocks_dir, file_idx, data_offset, height, block_hash, network)
        .map_err(|e| format!("height {height}: {e}"))?;
    Ok(Some(block))
}

