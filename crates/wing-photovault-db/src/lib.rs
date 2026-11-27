#![forbid(unsafe_code)]

pub mod error;
pub mod migrations;
pub mod repo;

pub use error::{DbError, DbResult};
pub use repo::{
    Database, ExistingFileSig, FindFilter, FindRow, GearHistoryReport, GearTimelineRow,
    LensAliasChange, NormalizeLensesReport, PairReport, ScanRunSummary, StatsReport, StatusReport,
    StorageStatsRow, UpsertFileInput, YearStatsRow,
};
