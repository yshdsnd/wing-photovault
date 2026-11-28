#![forbid(unsafe_code)]

pub mod error;
pub mod migrations;
pub mod repo;

pub use error::{DbError, DbResult};
pub use repo::{
    CameraCropSummary, CopyCandidateRow, Database, DerivativeAmbiguousRow, DerivativeLinkOptions,
    DerivativeLinkReport, DerivativeMatchRow, DuplicateFileRow, DuplicateFilter, DuplicateGroup,
    DuplicateReport, ExistingFileSig, FindCursor, FindFilter, FindPage, FindRow,
    Focal35DistributionReport, FocalBucketRow, FocalDistributionReport, FocalExactRow,
    GearHistoryReport, GearTimelineRow, JournalEntryInput, JournalFilter, JournalRow,
    LensAliasChange, NormalizeLensesReport, PairReport, ScanRunSummary, SeedBenchmarkReport,
    SidecarLinkReport, StatsFilter, StatsReport, StatusReport, StorageStatsRow, UpsertFileInput,
    VerifyCandidateRow, YearStatsRow,
};
