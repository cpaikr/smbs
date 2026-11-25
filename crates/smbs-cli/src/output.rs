//! `--output` files: CSV or XLSX rate tables, published atomically.

use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
};

use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
use serde::Serialize;
use sha2::{Digest, Sha256};
use smbs::{ErrorCode, SmbsError};

use crate::{
    CliError,
    table::{Period, Table},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    Csv,
    Xlsx,
}

/// A validated `--output` destination. Validation happens before any network
/// request, so a bad path never costs a fetch.
#[derive(Debug)]
pub struct OutputTarget {
    path: PathBuf,
    format: FileFormat,
    overwrite: bool,
}

/// Facts about a published file, reported in the JSON envelope.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub path: String,
    pub format: FileFormat,
    pub bytes: u64,
    pub sha256: String,
    pub rows: usize,
}

impl OutputTarget {
    /// # Errors
    /// `invalid_request` naming `output` for an unsupported extension, a
    /// missing parent directory, or an existing file without `overwrite`.
    pub fn new(path: &Path, overwrite: bool) -> Result<Self, SmbsError> {
        let invalid = |message: String, hint: &str| SmbsError {
            code: ErrorCode::InvalidRequest,
            message,
            retryable: false,
            parameter: Some("output".to_owned()),
            source_url: None,
            recovery_hint: Some(hint.to_owned()),
        };
        let format = match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("csv") => FileFormat::Csv,
            Some("xlsx") => FileFormat::Xlsx,
            _ => {
                return Err(invalid(
                    format!("Unsupported output file \"{}\".", path.display()),
                    "Use a path ending in .csv or .xlsx.",
                ));
            }
        };
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        if !parent.is_dir() {
            return Err(invalid(
                format!("The directory for \"{}\" does not exist.", path.display()),
                "Create the directory first.",
            ));
        }
        // symlink_metadata: a dangling link or an unreadable entry still
        // counts as something in the way, rather than as free space.
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(invalid(
                    format!("Cannot check \"{}\": {error}.", path.display()),
                    "Check the path and its permissions.",
                ));
            }
            Ok(metadata) if metadata.is_dir() => {
                return Err(invalid(
                    format!("\"{}\" is a directory.", path.display()),
                    "Choose a file path ending in .csv or .xlsx.",
                ));
            }
            Ok(_) if !overwrite => {
                return Err(invalid(
                    format!("\"{}\" already exists.", path.display()),
                    "Choose another path, or pass --overwrite to replace it.",
                ));
            }
            Ok(_) => {}
        }
        Ok(Self {
            path: path.to_owned(),
            format,
            overwrite,
        })
    }

    /// Renders `table` and publishes it: a temporary file in the target
    /// directory is fully written and flushed, then moved into place, so a
    /// failure never leaves a partial file at `path`.
    ///
    /// # Errors
    /// `output_failed` when rendering or any filesystem step fails.
    pub fn write(&self, table: &Table) -> Result<Artifact, CliError> {
        let bytes = match self.format {
            FileFormat::Csv => render_csv(table).into_bytes(),
            FileFormat::Xlsx => render_xlsx(table)
                .map_err(|error| self.failed(&format!("Building the workbook failed: {error}")))?,
        };
        self.publish(&bytes)?;
        let absolute = std::path::absolute(&self.path).unwrap_or_else(|_| self.path.clone());
        Ok(Artifact {
            path: absolute.display().to_string(),
            format: self.format,
            bytes: bytes.len() as u64,
            sha256: hex(&Sha256::digest(&bytes)),
            rows: table.rows.len(),
        })
    }

    /// Writes a uniquely named temporary file in the target directory,
    /// flushes it, then moves it into place. Without `--overwrite` the move
    /// refuses to replace anything (`MoveFileExW` without replace on Windows,
    /// `renameat2(RENAME_NOREPLACE)` or link-then-unlink on Unix), so a file
    /// created by another program after validation is never replaced. The
    /// temporary file is removed on every error path; only a killed process
    /// can leave one behind.
    fn publish(&self, bytes: &[u8]) -> Result<(), CliError> {
        let parent = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let write_failed = |error: std::io::Error| {
            self.failed(&format!("Writing the output file failed: {error}"))
        };
        let mut file = tempfile::Builder::new()
            .prefix(".smbs-")
            .suffix(".tmp")
            .tempfile_in(parent)
            .map_err(write_failed)?;
        file.write_all(bytes).map_err(write_failed)?;
        file.as_file().sync_all().map_err(write_failed)?;
        let persisted = if self.overwrite {
            file.persist(&self.path)
        } else {
            file.persist_noclobber(&self.path)
        };
        persisted.map(drop).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                self.failed("The output file appeared while smbs was running")
            } else {
                write_failed(error.error)
            }
        })
    }

    fn failed(&self, message: &str) -> CliError {
        CliError::output_failed(
            format!("{message} ({}).", self.path.display()),
            "Check the path and its permissions. On Windows, close the file if another program has it open.",
        )
    }
}

fn period_text(period: Period) -> String {
    match period {
        Period::Month(month) => month.to_string(),
        Period::Day(date) => date.to_string(),
    }
}

/// UTF-8 with a BOM so Excel shows Korean text correctly; CRLF per RFC 4180.
/// Periods and codes never contain separators, quotes, or line breaks.
fn render_csv(table: &Table) -> String {
    let mut text = String::from("\u{feff}");
    text.push_str(table.period_header);
    for header in &table.currency_headers {
        let _ = write!(text, ",{header}");
    }
    text.push_str("\r\n");
    for (period, cells) in &table.rows {
        text.push_str(&period_text(*period));
        for cell in cells {
            text.push(',');
            if let Some(rate) = cell {
                let _ = write!(text, "{rate}");
            }
        }
        text.push_str("\r\n");
    }
    text
}

fn render_xlsx(table: &Table) -> Result<Vec<u8>, rust_xlsxwriter::XlsxError> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name(table.name)?;
    let bold = Format::new().set_bold();
    let (date_format, width) = match table.rows.first() {
        Some((Period::Day(_), _)) => (Format::new().set_num_format("yyyy-mm-dd"), 12),
        _ => (Format::new().set_num_format("yyyy-mm"), 10),
    };
    sheet.write_string_with_format(0, 0, table.period_header, &bold)?;
    sheet.set_column_width(0, width)?;
    for (column, header) in (1u16..).zip(&table.currency_headers) {
        sheet.write_string_with_format(0, column, header, &bold)?;
        sheet.set_column_width(column, 12)?;
    }
    for (row, (period, cells)) in (1u32..).zip(&table.rows) {
        let date = match *period {
            Period::Month(month) => ExcelDateTime::from_ymd(month.year(), month.month(), 1)?,
            Period::Day(day) => ExcelDateTime::from_ymd(day.year(), day.month(), day.day())?,
        };
        sheet.write_datetime_with_format(row, 0, &date, &date_format)?;
        for (column, cell) in (1u16..).zip(cells) {
            if let Some(rate) = cell {
                sheet.write_number(row, column, *rate)?;
            }
        }
    }
    let note_row = u32::try_from(table.rows.len()).unwrap_or(u32::MAX - 2) + 2;
    sheet.write_string(
        note_row,
        0,
        format!(
            "Source: smbs.biz {} ({}). KRW per the unit in each header.",
            table.title, table.name
        ),
    )?;
    sheet.set_freeze_panes(1, 0)?;
    workbook.save_to_buffer()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use smbs::{Date, YearMonth};

    use super::*;

    fn sample(period: Period) -> Table {
        Table {
            name: "month-end",
            title: "월말 매매기준율",
            period_header: "month",
            currency_headers: vec!["USD".to_owned(), "JPY (100)".to_owned()],
            rows: vec![(period, vec![Some(1433.3), None])],
        }
    }

    #[test]
    fn csv_has_bom_crlf_and_empty_missing_cells() {
        let month = YearMonth::parse("2025-01", "m").unwrap();
        assert_eq!(
            render_csv(&sample(Period::Month(month))),
            "\u{feff}month,USD,JPY (100)\r\n2025-01,1433.3,\r\n"
        );
        let day = Date::parse("2025-03-31", "d").unwrap();
        assert!(render_csv(&sample(Period::Day(day))).contains("2025-03-31,1433.3,"));
    }

    #[test]
    fn xlsx_is_a_zip_workbook() {
        let month = YearMonth::parse("2025-01", "m").unwrap();
        let bytes = render_xlsx(&sample(Period::Month(month))).unwrap();
        assert!(bytes.starts_with(b"PK"));
    }

    #[test]
    fn target_validation_rejects_bad_paths_before_writing() {
        let directory =
            std::env::temp_dir().join(format!("smbs-output-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let existing = directory.join("rates.csv");
        std::fs::write(&existing, "keep").unwrap();
        let subdirectory = directory.join("folder.csv");
        std::fs::create_dir(&subdirectory).unwrap();
        for (path, overwrite) in [
            (subdirectory.clone(), false),
            (subdirectory, true),
            (directory.join("rates.txt"), false),
            (directory.join("rates"), false),
            (directory.join("missing").join("rates.csv"), false),
            (existing.clone(), false),
        ] {
            let error = OutputTarget::new(&path, overwrite).unwrap_err();
            assert_eq!(
                error.parameter.as_deref(),
                Some("output"),
                "{}",
                path.display()
            );
        }
        assert!(OutputTarget::new(&existing, true).is_ok());
        assert!(OutputTarget::new(&directory.join("RATES.XLSX"), false).is_ok());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn publishing_never_replaces_without_overwrite() {
        let directory =
            std::env::temp_dir().join(format!("smbs-output-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("rates.csv");
        let target = OutputTarget::new(&path, false).unwrap();
        // Another process creates the file after validation.
        std::fs::write(&path, "theirs").unwrap();
        let month = YearMonth::parse("2025-01", "m").unwrap();
        let error = target.write(&sample(Period::Month(month))).unwrap_err();
        assert!(
            error.message.contains("appeared while smbs was running"),
            "{}",
            error.message
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs");
        assert_eq!(
            std::fs::read_dir(&directory).unwrap().count(),
            1,
            "no temporary file left"
        );

        let replace = OutputTarget::new(&path, true).unwrap();
        let artifact = replace.write(&sample(Period::Month(month))).unwrap();
        assert_eq!(artifact.rows, 1);
        assert_eq!(artifact.bytes, std::fs::metadata(&path).unwrap().len());
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
