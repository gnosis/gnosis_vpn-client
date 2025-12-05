use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct DestinationTable {
    extra_headers: Vec<String>,
    rows: Vec<DestinationRow>,
}

impl DestinationTable {
    pub fn new(extra_headers: &[&str]) -> Self {
        Self {
            extra_headers: extra_headers.iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn add_row<S: Into<String>>(&mut self, label: S, status: RowStatus, values: Vec<String>) {
        self.rows.push(DestinationRow {
            label: label.into(),
            status,
            values,
        });
    }

    pub fn render(&self) -> String {
        if self.rows.is_empty() {
            return "(none)".to_string();
        }

        let mut headers = vec!["destination".to_string(), "status".to_string(), "details".to_string()];
        headers.extend(self.extra_headers.iter().cloned());

        let rows = self
            .rows
            .iter()
            .map(|row| {
                let mut cells = Vec::with_capacity(3 + row.values.len());
                cells.push(row.label.clone());
                cells.push(row.status.label());
                cells.push(row.status.detail());
                cells.extend(row.values.clone());
                cells
            })
            .collect::<Vec<_>>();

        format_table(&headers.iter().map(|s| s.as_str()).collect::<Vec<_>>(), &rows)
    }
}

#[derive(Debug, Clone)]
pub struct DestinationRow {
    label: String,
    status: RowStatus,
    values: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum RowStatus {
    Ready,
    NotReady,
    Success,
    Failure(String),
}

impl RowStatus {
    pub fn label(&self) -> String {
        match self {
            RowStatus::Ready => "ready".to_string(),
            RowStatus::NotReady => "not ready".to_string(),
            RowStatus::Success => "success".to_string(),
            RowStatus::Failure(_) => "failure".to_string(),
        }
    }

    pub fn detail(&self) -> String {
        match self {
            RowStatus::Failure(reason) => reason.clone(),
            _ => "-".to_string(),
        }
    }
}

fn format_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return "(none)".to_string();
    }

    let mut col_widths = headers.iter().map(|h| h.len()).collect::<Vec<_>>();
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            if idx < col_widths.len() {
                col_widths[idx] = col_widths[idx].max(cell.len());
            } else {
                col_widths.push(cell.len());
            }
        }
    }

    let mut table = String::new();
    write_table_row(&mut table, headers, &col_widths);
    write_table_separator(&mut table, &col_widths);

    for row in rows {
        let cell_refs = row.iter().map(|cell| cell.as_str()).collect::<Vec<_>>();
        write_table_row(&mut table, &cell_refs, &col_widths);
    }

    table
}

fn write_table_row(table: &mut String, cells: &[&str], col_widths: &[usize]) {
    table.push('|');
    for (idx, cell) in cells.iter().enumerate() {
        let width = col_widths.get(idx).copied().unwrap_or(cell.len());
        let _ = write!(table, " {:<width$} |", cell, width = width);
    }
    table.push('\n');
}

fn write_table_separator(table: &mut String, col_widths: &[usize]) {
    table.push('|');
    for width in col_widths {
        table.push(' ');
        for _ in 0..*width {
            table.push('-');
        }
        table.push(' ');
        table.push('|');
    }
    table.push('\n');
}
