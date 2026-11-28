#![forbid(unsafe_code)]

pub mod error;
pub mod migrations;
pub mod repo;

pub use error::{DbError, DbResult};
pub use repo::{
    Database, DuplicateFileRow, DuplicateFilter, DuplicateGroup, DuplicateReport, ExistingFileSig,
    FindCursor, FindFilter, FindPage, FindRow, GearHistoryReport, GearTimelineRow,
    LensAliasChange, NormalizeLensesReport, PairReport, ScanRunSummary, SeedBenchmarkReport,
    SidecarLinkReport, StatsReport, StatusReport, StorageStatsRow, UpsertFileInput, YearStatsRow,
};
