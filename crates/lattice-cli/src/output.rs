//! Output formatting — table, JSON, and wide formats.

use serde::Serialize;

/// Output format for CLI results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
    Wide,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "json" => OutputFormat::Json,
            "wide" => OutputFormat::Wide,
            _ => OutputFormat::Table,
        }
    }
}

/// A table row for formatted output.
pub struct TableRow {
    pub cells: Vec<String>,
}

/// Render a table to stdout.
pub fn render_table(headers: &[&str], rows: &[TableRow]) -> String {
    if rows.is_empty() {
        return "No results.".to_string();
    }

    // Calculate column widths
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.cells.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    let mut output = String::new();

    // Header
    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
        .collect();
    output.push_str(&header_line.join("  "));
    output.push('\n');

    // Separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    output.push_str(&sep.join("  "));
    output.push('\n');

    // Rows
    for row in rows {
        let cells: Vec<String> = row
            .cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let w = widths.get(i).copied().unwrap_or(c.len());
                format!("{:width$}", c, width = w)
            })
            .collect();
        output.push_str(&cells.join("  "));
        output.push('\n');
    }

    output
}

/// Render items as JSON.
pub fn render_json<T: Serialize>(items: &[T]) -> String {
    serde_json::to_string_pretty(items).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_parsing() {
        assert_eq!(OutputFormat::parse("json"), OutputFormat::Json);
        assert_eq!(OutputFormat::parse("JSON"), OutputFormat::Json);
        assert_eq!(OutputFormat::parse("wide"), OutputFormat::Wide);
        assert_eq!(OutputFormat::parse("table"), OutputFormat::Table);
        assert_eq!(OutputFormat::parse("unknown"), OutputFormat::Table);
    }

    #[test]
    fn empty_table() {
        let result = render_table(&["ID", "STATE"], &[]);
        assert_eq!(result, "No results.");
    }

    #[test]
    fn simple_table() {
        let rows = vec![
            TableRow {
                cells: vec!["abc-123".to_string(), "running".to_string()],
            },
            TableRow {
                cells: vec!["def-456".to_string(), "pending".to_string()],
            },
        ];

        let result = render_table(&["ID", "STATE"], &rows);
        assert!(result.contains("ID"));
        assert!(result.contains("STATE"));
        assert!(result.contains("abc-123"));
        assert!(result.contains("running"));
        assert!(result.contains("def-456"));
        assert!(result.contains("pending"));
    }

    #[test]
    fn table_column_widths() {
        let rows = vec![TableRow {
            cells: vec!["short".to_string(), "a-very-long-value".to_string()],
        }];

        let result = render_table(&["COL1", "COL2"], &rows);
        let lines: Vec<&str> = result.lines().collect();
        // All lines should have consistent structure
        assert!(lines.len() >= 3); // header + separator + 1 row
    }

    #[test]
    fn json_output() {
        let items = vec![
            serde_json::json!({"id": "123", "state": "running"}),
            serde_json::json!({"id": "456", "state": "pending"}),
        ];
        let result = render_json(&items);
        assert!(result.contains("123"));
        assert!(result.contains("running"));
    }
}
