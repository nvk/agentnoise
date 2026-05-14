pub fn format_chat_text(input: &str) -> String {
    let stripped = strip_ansi(input);
    let normalized = stripped.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>();
    let table_rewritten = rewrite_markdown_tables(&lines).join("\n");
    collapse_blank_lines(&table_rewritten).trim().to_string()
}

pub fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            output.push(ch);
            continue;
        }

        if chars.peek() == Some(&'[') {
            chars.next();
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() || code == '~' {
                    break;
                }
            }
        }
    }

    output
}

fn rewrite_markdown_tables(lines: &[String]) -> Vec<String> {
    let mut output = Vec::new();
    let mut index = 0;
    let mut in_fence = false;

    while index < lines.len() {
        let trimmed = lines[index].trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            output.push(lines[index].clone());
            index += 1;
            continue;
        }

        if !in_fence
            && is_table_row(&lines[index])
            && lines
                .get(index + 1)
                .is_some_and(|line| is_separator_row(line))
        {
            let headers = parse_table_row(&lines[index]);
            index += 2;

            if !headers.is_empty() {
                output.push(clean_table_cell(&headers.join(" / ")));
            }

            while index < lines.len() && is_table_row(&lines[index]) {
                let cells = parse_table_row(&lines[index]);
                if !cells.is_empty() {
                    output.push(format!("- {}", format_table_cells(&headers, &cells)));
                }
                index += 1;
            }
            continue;
        }

        output.push(lines[index].clone());
        index += 1;
    }

    output
}

fn collapse_blank_lines(input: &str) -> String {
    let mut output = Vec::new();
    let mut previous_blank = false;

    for line in input.lines() {
        let blank = line.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        output.push(line);
        previous_blank = blank;
    }

    output.join("\n")
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn is_separator_row(line: &str) -> bool {
    let cells = parse_table_row(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim();
            cell.len() >= 3 && cell.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
        })
}

fn parse_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .map(str::to_string)
        .collect()
}

fn format_table_cells(headers: &[String], cells: &[String]) -> String {
    let cleaned = cells
        .iter()
        .map(|cell| clean_table_cell(cell))
        .collect::<Vec<_>>();

    if cleaned.len() == 2 && headers.len() >= 2 {
        return format!("{}: {}", cleaned[0], cleaned[1]);
    }

    cleaned.join(" | ")
}

fn clean_table_cell(cell: &str) -> String {
    cell.replace('`', "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_markdown_table_for_phone() {
        let input = "| Topics |\n|---|\n| `alpha`, `beta` |\n| `gamma` |";

        assert_eq!(format_chat_text(input), "Topics\n- alpha, beta\n- gamma");
    }

    #[test]
    fn strips_ansi_and_collapses_blank_lines() {
        let input = "\x1b[31mError\x1b[0m\n\n\nnext";

        assert_eq!(format_chat_text(input), "Error\n\nnext");
    }
}
