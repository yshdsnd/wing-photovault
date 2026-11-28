#![forbid(unsafe_code)]

pub mod error;
pub mod migrations;
pub mod repo;

pub use error::{DbError, DbResult};
pub use repo::{
    CopyCandidateRow, Database, DuplicateFileRow, DuplicateFilter, DuplicateGroup,
    DuplicateReport, ExistingFileSig, FindCursor, FindFilter, FindPage, FindRow,
    FocalBucketRow, FocalDistributionReport, FocalExactRow, GearHistoryReport, GearTimelineRow,
    LensAliasChange, NormalizeLensesReport, PairReport, ScanRunSummary, SeedBenchmarkReport,
    SidecarLinkReport, StatsFilter, StatsReport, StatusReport, StorageStatsRow, UpsertFileInput,
    YearStatsRow,
};
