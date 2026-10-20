//! Text tables for list commands: pad by display width (CJK = 2 columns) and, on a
//! TTY with a known width, truncate one flexible column with `…`. Piped output is
//! never truncated.

use std::io::IsTerminal;
use unicode_width::UnicodeWidthStr;

pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Right-pad with spaces to `width` display columns (never truncates).
pub fn pad(s: &str, width: usize) -> String {
    let w = display_width(s);
    format!("{s}{}", " ".repeat(width.saturating_sub(w)))
}

/// Cut `s` to at most `max` display columns, ending in `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = display_width(ch.encode_utf8(&mut [0; 4]));
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if max > 0 {
        out.push('…');
    }
    out
}

/// Terminal width: only when stdout is a TTY and `COLUMNS` parses.
// ponytail: width comes only from COLUMNS (shells often don't export it); real TTY
// width -> add the terminal_size crate if users ask.
pub fn term_width() -> Option<usize> {
    if !std::io::stdout().is_terminal() {
        return None;
    }
    std::env::var("COLUMNS")
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|&w| w > 0)
}

/// Render a left-aligned table (last column unpadded, columns joined by two spaces).
/// `sep_last`: `Some(n)` adds a `─` rule under the header, `n` wide for the last
/// column. `flex`: the column to shrink to fit `width` (None width = never shrink).
pub fn render(
    headers: &[&str],
    rows: &[Vec<String>],
    sep_last: Option<usize>,
    flex: Option<usize>,
    width: Option<usize>,
) -> Vec<String> {
    let n = headers.len();
    let mut widths: Vec<usize> = (0..n)
        .map(|c| {
            rows.iter()
                .map(|r| display_width(&r[c]))
                .chain([display_width(headers[c])])
                .max()
                .unwrap()
        })
        .collect();
    if let (Some(total), Some(f)) = (width, flex) {
        let used = widths.iter().sum::<usize>() + 2 * (n - 1);
        if used > total {
            let others = used - widths[f];
            let min = display_width(headers[f]).max(1);
            widths[f] = total.saturating_sub(others).max(min).min(widths[f]);
        }
    }
    let line = |cells: Vec<String>| -> String {
        let cells: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let c = if flex == Some(i) {
                    truncate(c, widths[i])
                } else {
                    c.clone()
                };
                if i == n - 1 {
                    c
                } else {
                    pad(&c, widths[i])
                }
            })
            .collect();
        cells.join("  ")
    };
    let mut out = vec![line(headers.iter().map(|h| h.to_string()).collect())];
    if let Some(last) = sep_last {
        let rule: Vec<String> = (0..n)
            .map(|i| "─".repeat(if i == n - 1 { last } else { widths[i] }))
            .collect();
        out.push(rule.join("  "));
    }
    out.extend(rows.iter().map(|r| line(r.clone())));
    out
}

/// Print a table to stdout, shrinking `flex` (default: widest non-first column) to the
/// terminal width when one is known.
pub fn print(headers: &[&str], rows: &[Vec<String>], sep_last: Option<usize>, flex: Option<usize>) {
    use std::io::Write;
    let flex = flex.or_else(|| {
        // ponytail: widest non-first column (first is usually an id); explicit `flex` wins.
        (1..headers.len())
            .max_by_key(|&c| rows.iter().map(|r| display_width(&r[c])).max().unwrap_or(0))
            .or(Some(0))
    });
    for l in render(headers, rows, sep_last, flex, term_width()) {
        let _ = writeln!(std::io::stdout(), "{l}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Vec<String>> {
        vec![
            vec!["id1".into(), "한글제목".into(), "x".into()],
            vec!["id22".into(), "abc".into(), "y".into()],
        ]
    }

    #[test]
    fn pads_cjk_by_display_width() {
        assert_eq!(pad("한글", 6), "한글  ");
        assert_eq!(display_width("한글"), 4);
        let t = render(&["ID", "TITLE", "U"], &rows(), None, None, None);
        // TITLE column is 8 wide: "abc" gets 5 spaces, "한글제목" none.
        assert_eq!(t[1], "id1   한글제목  x");
        assert_eq!(t[2], "id22  abc       y");
    }

    #[test]
    fn no_truncation_when_width_unknown() {
        let t = render(&["ID", "TITLE", "U"], &rows(), None, Some(1), None);
        assert!(t[1].contains("한글제목"));
    }

    #[test]
    fn truncates_flex_column_with_ellipsis_at_width() {
        let t = render(&["ID", "TITLE", "U"], &rows(), None, Some(1), Some(14));
        for l in &t {
            assert!(display_width(l) <= 14, "{l:?}");
        }
        assert!(t[1].contains('…'));
        assert_eq!(truncate("한글제목", 5), "한글…");
        assert_eq!(truncate("short", 5), "short");
    }

    #[test]
    fn separator_row() {
        let t = render(
            &["A", "B"],
            &[vec!["1".into(), "2".into()]],
            Some(16),
            None,
            None,
        );
        assert_eq!(t[1], format!("─  {}", "─".repeat(16)));
    }
}
