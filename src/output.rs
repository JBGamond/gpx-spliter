use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

use crate::chunker::Chunk;

/// Format a float using a comma as the decimal separator (European convention).
fn fmt_f64(v: f64) -> String {
    format!("{}", v).replace('.', ",")
}

/// Write chunks to a CSV file (or stdout if path is "-").
pub fn write_csv(chunks: &[Chunk], path: &Path) -> Result<()> {
    let writer: Box<dyn Write> = if path.as_os_str() == "-" {
        Box::new(std::io::stdout().lock())
    } else {
        Box::new(
            std::fs::File::create(path)
                .with_context(|| format!("Failed to create output file: {}", path.display()))?,
        )
    };
    write_csv_to_writer(chunks, writer)
}

/// Write chunks to a generic writer in CSV format.
pub fn write_csv_to_writer<W: Write>(chunks: &[Chunk], writer: W) -> Result<()> {
    let mut wtr = csv::WriterBuilder::new()
        .delimiter(b';')
        .from_writer(writer);

    // Write header manually so we control field names.
    wtr.write_record(&[
        "chunk_number",
        "distance_km",
        "cumulative_km",
        "elevation_gain_m",
        "elevation_loss_m",
        "cumulative_gain_m",
        "cumulative_loss_m",
        "gradient_pct",
        "classification",
        "fatigue_pct",
        "estimated_pace",
        "estimated_time",
        "cumulative_time",
    ])
    .context("Failed to write CSV header")?;

    for chunk in chunks {
        wtr.write_record(&[
            chunk.chunk_number.to_string(),
            format!("{:.3}", chunk.distance_km).replace('.', ","),
            format!("{:.3}", chunk.cumulative_km).replace('.', ","),
            fmt_f64(chunk.elevation_gain_m),
            fmt_f64(chunk.elevation_loss_m),
            fmt_f64(chunk.cumulative_gain_m),
            fmt_f64(chunk.cumulative_loss_m),
            format!("{:.2}", chunk.gradient_pct).replace('.', ","),
            chunk.classification.clone(),
            format!("{:.1}", chunk.fatigue_pct).replace('.', ","),
            chunk.estimated_pace.clone().unwrap_or_default(),
            chunk.estimated_time.clone().unwrap_or_default(),
            chunk.cumulative_time.clone().unwrap_or_default(),
        ])
        .with_context(|| format!("Failed to write chunk {}", chunk.chunk_number))?;
    }

    wtr.flush().context("Failed to flush CSV writer")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk() -> Chunk {
        Chunk {
            chunk_number: 1,
            distance_km: 1.234,
            cumulative_km: 1.234,
            elevation_gain_m: 10.0,
            elevation_loss_m: 0.0,
            cumulative_gain_m: 10.0,
            cumulative_loss_m: 0.0,
            gradient_pct: 2.5,
            classification: "gentle uphill".to_string(),
            fatigue_pct: 5.5,
            estimated_pace: Some("06:00".to_string()),
            estimated_time: Some("00:07:24".to_string()),
            cumulative_time: Some("00:07:24".to_string()),
        }
    }

    /// Regression test: the header must have exactly as many fields as each
    /// data row, or the `csv` crate refuses to write past the first record.
    #[test]
    fn header_and_row_have_matching_field_count() {
        let mut buffer = Vec::new();
        write_csv_to_writer(&[make_chunk(), make_chunk()], &mut buffer).unwrap();
        let text = String::from_utf8(buffer).unwrap();
        let mut lines = text.lines();
        let header_fields = lines.next().unwrap().split(';').count();
        let row_fields = lines.next().unwrap().split(';').count();
        assert_eq!(header_fields, row_fields);
    }

    #[test]
    fn fatigue_pct_is_rounded_and_uses_comma_decimal() {
        let mut buffer = Vec::new();
        write_csv_to_writer(&[make_chunk()], &mut buffer).unwrap();
        let text = String::from_utf8(buffer).unwrap();
        let row = text.lines().nth(1).unwrap();
        assert!(row.contains(";5,5;"), "expected fatigue field '5,5' in row: {row}");
    }
}

