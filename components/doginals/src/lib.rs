//! Types for interoperating with doginals, inscriptions, and dunes.
#![allow(clippy::large_enum_variant)]

use {
    bitcoin::{
        consensus::{Decodable, Encodable},
        opcodes,
        script::{self, Instruction},
        Network, OutPoint, ScriptBuf, Transaction,
    },
    derive_more::{Display, FromStr},
    serde::{Deserialize, Serialize},
    serde_with::{DeserializeFromStr, SerializeDisplay},
    std::{
        cmp,
        collections::{HashMap, VecDeque},
        fmt::{self, Formatter},
        num::ParseIntError,
        ops::{Add, AddAssign, Sub},
        sync::LazyLock,
    },
    thiserror::Error,
};

pub use {
    artifact::Artifact, cenotaph::Cenotaph, decimal_koinu::DecimalKoinu, degree::Degree,
    dogespell::Dogespell, dune::Dune, dune_id::DuneId, dunestone::Dunestone, edict::Edict,
    envelope::Envelope, epoch::Epoch, etching::Etching, flaw::Flaw, height::Height,
    inscription::Inscription, inscription_id::InscriptionId, koinu::Koinu, koinu_point::KoinuPoint,
    media::Media, pile::Pile, rarity::Rarity, spaced_dune::SpacedDune, tag::Tag, terms::Terms,
};

pub const COIN_VALUE: u64 = 100_000_000;
pub const CYCLE_EPOCHS: u32 = 6;

// Dogecoin-specific chain constants.
pub const DIFFCHANGE_INTERVAL: u32 = 1;

/// The actual Dogecoin subsidy halving interval: every 100,000 blocks after
/// the wonky era.
///
/// Source: `dogecoin/src/chainparams.cpp` `nSubsidyHalvingInterval`.
pub const DOGECOIN_HALVING_INTERVAL: u32 = 100_000;

/// Internal epoch machinery constant.
///
/// Set to 1 so that `Epoch(n)` maps 1-to-1 with block height `n`.  This lets
/// the existing doginals epoch machinery work unmodified while `Epoch::subsidy`
/// and `Epoch::starting_sat` delegate to the Dogecoin-specific subsidy
/// functions in `epoch.rs`.  The *actual* Dogecoin halving interval is
/// `DOGECOIN_HALVING_INTERVAL`.
pub const SUBSIDY_HALVING_INTERVAL: u32 = 1;

fn default<T: Default>() -> T {
    Default::default()
}

mod artifact;
mod cenotaph;
mod decimal_koinu;
mod degree;
pub mod dogespell;
mod dune;
mod dune_id;
mod dunestone;
mod edict;
pub mod envelope;
mod epoch;
mod etching;
mod flaw;
pub mod height;
pub mod inscription;
pub mod inscription_id;
pub mod koinu;
pub mod koinu_point;
pub mod media;
mod pile;
pub mod rarity;
pub mod spaced_dune;
pub mod tag;
mod terms;
pub mod varint;
