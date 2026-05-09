pub const FORMAT_TABLE: &str = "table";
pub const FORMAT_PLAIN: &str = "plain";
pub const FORMAT_JSON: &str = "json";
pub const FORMAT_SUMMARY: &str = "summary";
pub const FORMATS: [&str; 4] = [FORMAT_TABLE, FORMAT_PLAIN, FORMAT_JSON, FORMAT_SUMMARY];

pub const ABANDONED_IGNORE: &str = "ignore";
pub const ABANDONED_REPORT: &str = "report";
pub const ABANDONED_FAIL: &str = "fail";
pub const ABANDONEDS: [&str; 3] = [ABANDONED_IGNORE, ABANDONED_REPORT, ABANDONED_FAIL];

/// Mirrors `Auditor::FORMAT_*` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuditFormat {
    #[default]
    Table,
    Plain,
    Json,
    Summary,
}

impl AuditFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => FORMAT_TABLE,
            Self::Plain => FORMAT_PLAIN,
            Self::Json => FORMAT_JSON,
            Self::Summary => FORMAT_SUMMARY,
        }
    }
}

impl std::str::FromStr for AuditFormat {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            FORMAT_TABLE => Ok(Self::Table),
            FORMAT_PLAIN => Ok(Self::Plain),
            FORMAT_JSON => Ok(Self::Json),
            FORMAT_SUMMARY => Ok(Self::Summary),
            _ => Err(()),
        }
    }
}

/// Mirrors `Auditor::ABANDONED_*` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AbandonedHandling {
    Ignore,
    Report,
    #[default]
    Fail,
}

impl AbandonedHandling {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ignore => ABANDONED_IGNORE,
            Self::Report => ABANDONED_REPORT,
            Self::Fail => ABANDONED_FAIL,
        }
    }
}

impl std::str::FromStr for AbandonedHandling {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            ABANDONED_IGNORE => Ok(Self::Ignore),
            ABANDONED_REPORT => Ok(Self::Report),
            ABANDONED_FAIL => Ok(Self::Fail),
            _ => Err(()),
        }
    }
}
