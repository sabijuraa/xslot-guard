//! Shared types for the analysis CLI.

use serde::{Deserialize, Serialize};

/// A single swap event reconstructed from on-chain data.
///
/// Prices are stored as `f64` here (off-chain only) for ergonomics; they are
/// converted to [`xslot_core::FixedPrice`] before entering the guard so the
/// replay exercises the exact on-chain math.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SwapEvent {
    /// Slot in which the swap executed.
    pub slot: u64,
    /// Transaction signature (for traceability back to the explorer).
    pub signature: String,
    /// Execution price (quote per base) as a positive float.
    pub price: f64,
    /// Whether this event has been independently labelled as part of a
    /// sandwich attack (used to score detection precision/recall on labelled
    /// datasets). `None` for unlabelled live data.
    #[serde(default)]
    pub is_attack: Option<bool>,
}

/// The outcome of replaying a stream of swaps through the guard.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalysisReport {
    /// Total swaps examined (excludes warmup swaps the guard could not judge).
    pub evaluated: u64,
    /// Swaps the guard allowed.
    pub allowed: u64,
    /// Swaps the guard rejected (flagged as cross-slot manipulation).
    pub rejected: u64,
    /// Swaps skipped because the oracle was still warming up.
    pub warmup_skipped: u64,
    /// Estimated compute units the guard adds per swap (see `cu` module).
    pub cu_overhead_per_swap: u64,

    /// Confusion-matrix counts, populated only when the input is labelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labelled: Option<LabelledScore>,
}

/// Detection scoring against a labelled dataset.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LabelledScore {
    /// Attacks correctly rejected.
    pub true_positives: u64,
    /// Honest swaps wrongly rejected.
    pub false_positives: u64,
    /// Attacks wrongly allowed.
    pub false_negatives: u64,
    /// Honest swaps correctly allowed.
    pub true_negatives: u64,
}

impl LabelledScore {
    /// Detection rate = recall = TP / (TP + FN).
    pub fn detection_rate(&self) -> f64 {
        let denom = self.true_positives + self.false_negatives;
        if denom == 0 {
            return 0.0;
        }
        self.true_positives as f64 / denom as f64
    }

    /// Precision = TP / (TP + FP).
    pub fn precision(&self) -> f64 {
        let denom = self.true_positives + self.false_positives;
        if denom == 0 {
            return 0.0;
        }
        self.true_positives as f64 / denom as f64
    }

    /// False-positive rate = FP / (FP + TN).
    pub fn false_positive_rate(&self) -> f64 {
        let denom = self.false_positives + self.true_negatives;
        if denom == 0 {
            return 0.0;
        }
        self.false_positives as f64 / denom as f64
    }
}
